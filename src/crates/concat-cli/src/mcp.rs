// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The Model Context Protocol transport: the Demon Cut API as tools an AI
//! agent can call, spoken over stdin and stdout the way MCP's stdio
//! transport asks - one JSON-RPC message a line in, one a line out.
//!
//! It is the third transport over the same [`Api`], beside the `api`
//! command's lines and `serve`'s sockets, and it reinterprets nothing:
//! each tool is one API method, its arguments are that method's fields as
//! they are, and its result is the method's reply as JSON. Two tools are
//! the transport's own. `edit_reference` hands the agent the edit
//! vocabulary, read from `concat-project`'s source at build time so it
//! never drifts from what `edit.apply` accepts. `export_status` reports
//! the events a running export sends, and can wait for its end, because an
//! agent makes calls and does not read a stream.
//!
//! An API refusal is a tool result marked `isError`, with the sentence the
//! window would show, so the agent reads why and tries again; only a
//! message that is not MCP at all gets a JSON-RPC error. Writes are
//! confined to the home folder unless `--root` says otherwise, as `serve`
//! confines them: the caller here is a model, not a person at a terminal.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use concat_api::{Api, Event, Reply, Request, Response};
use serde_json::{Map, Value, json};

/// The protocol revisions this server speaks, newest first. A client that
/// asks for one of them gets it; any other gets the newest.
const PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// The longest `export_status` will hold a call open, in seconds.
const MAX_WAIT: f64 = 600.0;

/// The edit commands' source, notes and all: the vocabulary
/// `edit_reference` hands an agent.
const COMMANDS_SOURCE: &str = include_str!("../../concat-project/src/commands/mod.rs");

/// The latest event of every job, by job name, and a signal for each new
/// one, shared between the API's job threads and the call loop.
type Jobs = Arc<(Mutex<BTreeMap<String, Value>>, Condvar)>;

/// What a tool does when it is called.
enum Kind {
    /// One API method, named as the API names it.
    Api(&'static str),
    /// The edit vocabulary.
    EditReference,
    /// A job's latest event, waited for when asked.
    ExportStatus,
}

/// One tool, as `tools/list` describes it.
struct Tool {
    /// The tool's name.
    name: &'static str,
    /// What calling it does.
    kind: Kind,
    /// Whether it changes nothing: a hint the client may use to call it
    /// without asking the person first.
    read_only: bool,
    /// What it is for, for the model reading the list.
    description: &'static str,
    /// The JSON Schema of its arguments.
    input: Value,
}

/// Runs the MCP server until stdin ends, then waits for any running job.
pub fn run(roots: Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    let jobs: Jobs = Arc::default();
    let sink = Arc::clone(&jobs);
    let mut api = Api::new(Arc::new(move |event: Event| {
        let finished = matches!(event, Event::ExportDone { .. } | Event::ExportFailed { .. });
        let job = event.job().to_owned();
        let value = serde_json::to_value(&event).unwrap_or(Value::Null);
        {
            let (latest, signal) = &*sink;
            lock(latest).insert(job, value.clone());
            signal.notify_all();
        }
        // The end of a job is worth a line in the client's log; every
        // frame of progress is not.
        if finished {
            emit(&json!({
                "jsonrpc": "2.0",
                "method": "notifications/message",
                "params": { "level": "info", "logger": "demoncut", "data": value },
            }));
        }
    }))?;
    api.add_capability("mcp");
    let roots = if roots.is_empty() {
        home().into_iter().collect()
    } else {
        roots
    };
    api.restrict_writes_to(roots);

    let mut server = Server {
        api,
        jobs,
        tools: tools(),
    };
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let replies: Vec<Value> =
                    batch.iter().filter_map(|one| server.handle(one)).collect();
                (!replies.is_empty()).then_some(Value::Array(replies))
            }
            Ok(message) => server.handle(&message),
            Err(error) => Some(failure(Value::Null, -32700, &error.to_string())),
        };
        if let Some(reply) = reply {
            emit(&reply);
        }
    }
    server.api.finish();
    Ok(())
}

/// The API and what the transport keeps beside it.
struct Server {
    api: Api,
    jobs: Jobs,
    tools: Vec<Tool>,
}

impl Server {
    /// Answers one message; a notification, or a response to a request
    /// this server never makes, gets nothing.
    fn handle(&mut self, message: &Value) -> Option<Value> {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            let is_response = message.get("result").is_some() || message.get("error").is_some();
            return (!is_response).then(|| {
                let id = message.get("id").cloned().unwrap_or(Value::Null);
                failure(id, -32600, "a request names its method")
            });
        };
        // Without an id it is a notification: `initialized`, `cancelled`.
        let id = message.get("id")?.clone();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(initialize(&params)),
            "ping" | "logging/setLevel" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.list() })),
            "tools/call" => self.call(&params),
            "resources/list" => Ok(json!({ "resources": [] })),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            other => Err((-32601, format!("there is no method {other}"))),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => failure(id, code, &message),
        })
    }

    /// `tools/list`.
    fn list(&self) -> Vec<Value> {
        self.tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input,
                    "annotations": { "readOnlyHint": tool.read_only },
                })
            })
            .collect()
    }

    /// `tools/call`.
    fn call(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((-32602, "tools/call names its tool".to_owned()))?;
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(arguments)) => arguments.clone(),
            Some(_) => return Err((-32602, "arguments is an object".to_owned())),
        };
        let kind = match self.tools.iter().find(|tool| tool.name == name) {
            Some(tool) => match tool.kind {
                Kind::Api(method) => Some(method),
                Kind::EditReference => return Ok(text(edit_reference(), false)),
                Kind::ExportStatus => return Ok(self.export_status(&arguments)),
            },
            None => None,
        };
        let method = kind.ok_or((-32602, format!("there is no tool named {name}")))?;
        Ok(self.dispatch(method, arguments))
    }

    /// One API method, with the tool's arguments as its fields.
    fn dispatch(&mut self, method: &str, mut arguments: Map<String, Value>) -> Value {
        arguments.insert("method".to_owned(), Value::String(method.to_owned()));
        let request = match serde_json::from_value::<Request>(Value::Object(arguments)) {
            Ok(request) => request,
            Err(error) => return text(format!("{method}: {error}"), true),
        };
        match self.api.dispatch(request) {
            // An inline frame comes back as an image the model can look at.
            Response::Result(Reply::Picture(picture)) => json!({
                "content": [
                    { "type": "image", "data": picture.png, "mimeType": "image/png" },
                    { "type": "text", "text": format!("{}x{} frame", picture.width, picture.height) },
                ],
                "isError": false,
            }),
            Response::Result(reply) => {
                let value = serde_json::to_value(&reply).unwrap_or(Value::Null);
                text(value.to_string(), false)
            }
            Response::Error(error) => {
                let code = serde_json::to_value(error.code).unwrap_or(Value::Null);
                text(format!("{} (code: {code})", error.message), true)
            }
        }
    }

    /// `export_status`: one job's latest event, waiting up to `waitSeconds`
    /// for it to end, or every job's when none is named.
    fn export_status(&self, arguments: &Map<String, Value>) -> Value {
        let job = arguments.get("job").and_then(Value::as_str);
        let wait = arguments
            .get("waitSeconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .clamp(0.0, MAX_WAIT);
        let deadline = Instant::now() + Duration::from_secs_f64(wait);
        let (latest, signal) = &*self.jobs;
        let mut map = lock(latest);
        if let Some(job) = job {
            loop {
                let ended = map.get(job).is_some_and(ended);
                let now = Instant::now();
                if ended || now >= deadline {
                    break;
                }
                map = match signal.wait_timeout(map, deadline - now) {
                    Ok((map, _)) => map,
                    Err(poisoned) => poisoned.into_inner().0,
                };
            }
            return match map.get(job) {
                Some(event) => text(event.to_string(), false),
                None => text(format!("no events from {job} yet"), false),
            };
        }
        if map.is_empty() {
            return text("no jobs have reported yet".to_owned(), false);
        }
        let every: Map<String, Value> = map
            .iter()
            .map(|(job, event)| (job.clone(), event.clone()))
            .collect();
        text(Value::Object(every).to_string(), false)
    }
}

/// Whether an event is the last its job sends.
fn ended(event: &Value) -> bool {
    matches!(
        event.get("event").and_then(Value::as_str),
        Some("export.done" | "export.failed")
    )
}

/// The reply to `initialize`.
fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|asked| PROTOCOLS.contains(asked))
        .unwrap_or(PROTOCOLS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false }, "logging": {} },
        "serverInfo": {
            "name": "demoncut",
            "title": "Demon Cut",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": INSTRUCTIONS,
    })
}

/// What the agent is told when it connects.
const INSTRUCTIONS: &str = "Demon Cut is a local video editor. A project is a folder; \
every project tool names it by its absolute path, and a project must be opened \
(project_open or project_create) before it is edited. Typical flow: project_create or \
project_open, media_import each file, edit_apply commands (call edit_reference once for \
the command vocabulary; project_get shows track, media and clip ids), preview_frame to \
look at the result, project_save, then export_run and export_status with waitSeconds \
until export.done. Times are seconds. Edits are undoable with edit_undo. Writes are \
confined to the user's home folder. The editor window and this server do not share an \
open project: close it in one before opening it in the other.";

/// The edit vocabulary for `edit_reference`: how a command is spelled,
/// then every command with its notes, as `concat-project` defines them.
fn edit_reference() -> String {
    let start = COMMANDS_SOURCE
        .find("/// Every edit, as the window sends it")
        .unwrap_or(0);
    let end = COMMANDS_SOURCE[start..]
        .find("pub enum CommandError")
        .map_or(COMMANDS_SOURCE.len(), |end| start + end);
    format!(
        "edit_apply takes `command`: a JSON object with an `op` naming the variant in \
camelCase (AddClip is \"addClip\") and the variant's fields in camelCase (media_id is \
\"mediaId\"). Example: {{\"op\": \"addClip\", \"mediaId\": \"m1\", \"trackId\": \"t1\", \
\"start\": 0}}. Several edits at once, undone as one: {{\"op\": \"batch\", \"commands\": \
[...]}}. Types the fields name (clip patches, text styles, transforms) are spelled as \
project_document prints them. The commands, from concat-project's source:\n\n{}",
        &COMMANDS_SOURCE[start..end]
    )
}

/// A tool result of one text block.
fn text(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// A JSON-RPC error reply.
fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// One message to stdout, whole and flushed. Stdout carries nothing else.
fn emit(message: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

/// A lock that a panicked job thread cannot keep closed.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The user's home folder, where writes are confined by default.
fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// An object schema from its properties and the names it requires.
fn schema(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

/// The `path` every project tool takes.
fn folder() -> Value {
    json!({ "type": "string", "description": "The project folder, as an absolute path." })
}

/// A frame size and rate, as `project.create` and `project.setVideo` take it.
fn video() -> Value {
    json!({
        "type": "object",
        "description": "Frame and rate, e.g. {\"width\": 1080, \"height\": 1920, \"rateNum\": 30, \"rateDen\": 1} for vertical 1080p30.",
        "properties": {
            "width": { "type": "integer" },
            "height": { "type": "integer" },
            "rateNum": { "type": "integer" },
            "rateDen": { "type": "integer" },
        },
        "required": ["width", "height", "rateNum", "rateDen"],
    })
}

/// Every tool this server offers.
fn tools() -> Vec<Tool> {
    let tool = |name: &'static str,
                kind: Kind,
                read_only: bool,
                description: &'static str,
                input: Value| Tool {
        name,
        kind,
        read_only,
        description,
        input,
    };
    let path_only = || schema(json!({ "path": folder() }), &["path"]);
    vec![
        tool(
            "version",
            Kind::Api("version"),
            true,
            "The API version, the build behind it, its data folders and capabilities.",
            schema(json!({}), &[]),
        ),
        tool(
            "project_list",
            Kind::Api("project.list"),
            true,
            "The projects this machine opened most recently, newest first.",
            schema(json!({}), &[]),
        ),
        tool(
            "project_create",
            Kind::Api("project.create"),
            false,
            "Creates a project folder named `name` inside `location` and opens it. 1080p at 30 when `video` is absent.",
            schema(
                json!({
                    "location": { "type": "string", "description": "The directory the project folder is made in." },
                    "name": { "type": "string", "description": "The project's name; the folder is named after it." },
                    "video": video(),
                }),
                &["location", "name"],
            ),
        ),
        tool(
            "project_open",
            Kind::Api("project.open"),
            false,
            "Opens a project folder and returns its state: timelines, tracks, media and clips with their ids.",
            path_only(),
        ),
        tool(
            "project_get",
            Kind::Api("project.get"),
            true,
            "The state of an open project: timelines, tracks, media and clips with their ids.",
            path_only(),
        ),
        tool(
            "project_document",
            Kind::Api("project.document"),
            true,
            "An open project's document exactly as a save would write it.",
            path_only(),
        ),
        tool(
            "project_save",
            Kind::Api("project.save"),
            false,
            "Writes an open project's document to its folder, renaming it when `name` is given.",
            schema(
                json!({ "path": folder(), "name": { "type": "string" } }),
                &["path"],
            ),
        ),
        tool(
            "project_close",
            Kind::Api("project.close"),
            false,
            "Closes an open project. Unsaved edits are dropped unless `save` is true.",
            schema(
                json!({ "path": folder(), "save": { "type": "boolean" } }),
                &["path"],
            ),
        ),
        tool(
            "project_set_video",
            Kind::Api("project.setVideo"),
            false,
            "Sets the active timeline's frame size and rate, as an undoable edit.",
            schema(
                json!({ "path": folder(), "video": video() }),
                &["path", "video"],
            ),
        ),
        tool(
            "edit_apply",
            Kind::Api("edit.apply"),
            false,
            "Applies one edit command to an open project: add, move, trim, split or remove clips, titles, tracks, keyframes, effects, speed and more. Call edit_reference first for the command vocabulary.",
            schema(
                json!({
                    "path": folder(),
                    "command": { "type": "object", "description": "The edit: {\"op\": \"<camelCase command>\", ...camelCase fields}. See edit_reference." },
                }),
                &["path", "command"],
            ),
        ),
        tool(
            "edit_undo",
            Kind::Api("edit.undo"),
            false,
            "Steps an open project's history back one edit.",
            path_only(),
        ),
        tool(
            "edit_redo",
            Kind::Api("edit.redo"),
            false,
            "Steps an open project's history forward one edit.",
            path_only(),
        ),
        tool(
            "edit_reference",
            Kind::EditReference,
            true,
            "The full edit command vocabulary edit_apply accepts, with each command's fields and rules.",
            schema(json!({}), &[]),
        ),
        tool(
            "media_probe",
            Kind::Api("media.probe"),
            true,
            "Reports what is inside a media file - streams, duration, size, rate - without touching a project.",
            schema(
                json!({ "path": { "type": "string", "description": "The media file, as an absolute path." } }),
                &["path"],
            ),
        ),
        tool(
            "media_import",
            Kind::Api("media.import"),
            false,
            "Adds a video, audio or image file to an open project's bin, minting its media id.",
            schema(
                json!({
                    "path": folder(),
                    "file": { "type": "string", "description": "The file to import, as an absolute path." },
                }),
                &["path", "file"],
            ),
        ),
        tool(
            "catalogue_list",
            Kind::Api("catalogue.list"),
            true,
            "Every effect, filter, audio effect, transition and generator, with its id and parameters.",
            schema(
                json!({ "kind": { "type": "string", "enum": ["effect", "filter", "audio", "transition", "generator"] } }),
                &[],
            ),
        ),
        tool(
            "template_list",
            Kind::Api("template.list"),
            true,
            "The template library.",
            schema(json!({}), &[]),
        ),
        tool(
            "template_instantiate",
            Kind::Api("template.instantiate"),
            false,
            "Makes and opens a project from a template, with a file for every slot.",
            schema(
                json!({
                    "template": { "type": "string", "description": "The template bundle folder." },
                    "location": { "type": "string", "description": "The directory the project folder is made in." },
                    "name": { "type": "string" },
                    "fills": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": { "mediaId": { "type": "string" }, "file": { "type": "string" } },
                            "required": ["mediaId", "file"],
                        },
                    },
                }),
                &["template", "location", "name", "fills"],
            ),
        ),
        tool(
            "template_save",
            Kind::Api("template.save"),
            false,
            "Packs an open project into a new template.",
            schema(
                json!({ "path": folder(), "name": { "type": "string" } }),
                &["path", "name"],
            ),
        ),
        tool(
            "export_run",
            Kind::Api("export.run"),
            false,
            "Starts rendering an open project's active timeline to a video file and returns the job name at once. Follow it with export_status. One export runs at a time.",
            schema(
                json!({
                    "path": folder(),
                    "output": { "type": "string", "description": "The file to write, e.g. C:\\Users\\me\\Videos\\out.mp4." },
                    "crf": { "type": "integer", "description": "Quality; lower is better and bigger. 20 when absent." },
                    "preset": { "type": "string", "description": "x264 preset. medium when absent." },
                    "width": { "type": "integer" },
                    "height": { "type": "integer" },
                    "rateNum": { "type": "integer" },
                    "rateDen": { "type": "integer" },
                    "codec": { "type": "string", "enum": ["h264", "hevc", "av1"] },
                    "tenBit": { "type": "boolean" },
                    "colorRange": { "type": "string", "enum": ["limited", "full"] },
                    "hdr": { "type": "boolean" },
                }),
                &["path", "output"],
            ),
        ),
        tool(
            "export_cancel",
            Kind::Api("export.cancel"),
            false,
            "Stops a running export at its next frame.",
            schema(json!({ "job": { "type": "string" } }), &["job"]),
        ),
        tool(
            "export_status",
            Kind::ExportStatus,
            true,
            "A job's latest progress, or how it ended (export.done with the file, or export.failed with why). With waitSeconds (up to 600) the call waits for the job to end. With no job, every job's latest event.",
            schema(
                json!({
                    "job": { "type": "string", "description": "The job export_run named, e.g. j1." },
                    "waitSeconds": { "type": "number" },
                }),
                &[],
            ),
        ),
        tool(
            "preview_frame",
            Kind::Api("preview.frame"),
            true,
            "Renders the true composited frame at `time` seconds. Without `output` the picture comes back as an image to look at; with it, a PNG is written there.",
            schema(
                json!({
                    "path": folder(),
                    "time": { "type": "number", "description": "The timeline instant, in seconds." },
                    "output": { "type": "string" },
                    "width": { "type": "integer", "description": "Smaller is faster; e.g. 640." },
                    "height": { "type": "integer" },
                }),
                &["path", "time"],
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_api_tool_names_a_method_the_api_knows() {
        for tool in tools() {
            if let Kind::Api(method) = tool.kind {
                let error = serde_json::from_value::<Request>(json!({ "method": method }))
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_default();
                assert!(
                    !error.contains("unknown variant"),
                    "{}: {method} is not an API method",
                    tool.name
                );
            }
        }
    }

    #[test]
    fn the_edit_reference_holds_the_commands() {
        let reference = edit_reference();
        assert!(reference.contains("pub enum Command"));
        assert!(reference.contains("AddClip"));
        assert!(!reference.contains("pub enum CommandError"));
    }

    #[test]
    fn initialize_agrees_on_a_version() {
        let reply = initialize(&json!({ "protocolVersion": "2025-03-26" }));
        assert_eq!(reply["protocolVersion"], "2025-03-26");
        let reply = initialize(&json!({ "protocolVersion": "1999-01-01" }));
        assert_eq!(reply["protocolVersion"], PROTOCOLS[0]);
    }
}
