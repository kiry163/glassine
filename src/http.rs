//! HTTP framing, routing, and the response envelopes.

use crate::status::{mode_name, StatusSnapshot};

/// The body ceiling from the spec, used by the server that calls this.
pub const MAX_BODY_BYTES: usize = 65536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// How a body is to be read, decided from `Content-Type`.
///
/// `route` cannot do its job without this: `text/plain` means the body *is* the
/// text, while `application/json` means the text is a field inside it, and the
/// content type is the only thing that distinguishes the two spellings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentKind {
    Text,
    Json,
    /// Anything else, read as plain text (spec 9.2).
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub path: String,
    pub body: Vec<u8>,
    pub content_type: ContentKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    SetText(String),
    SetTime,
    Quit,
    Status,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

/// Parses one HTTP/1.1 request.
///
/// Framing only, plus the body checks the spec attaches to the content type: a
/// declared length over `max_body` is refused *before* the body is read, and a
/// body that is neither valid UTF-8 nor the JSON object `application/json`
/// promises is refused before routing.
pub fn parse_request(raw: &[u8], max_body: usize) -> Result<Request, HttpError> {
    let boundary = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| bad_request("no header/body separator"))?;
    let head = std::str::from_utf8(&raw[..boundary]).map_err(|_| invalid_utf8())?;

    let mut request_line = head.split("\r\n").next().unwrap_or_default().split(' ');
    let method = match request_line.next().unwrap_or_default() {
        "GET" => Method::Get,
        "POST" => Method::Post,
        // A known method on the wrong endpoint is routing's business; an
        // unrecognised one cannot be routed at all.
        _ => return Err(method_not_allowed()),
    };
    let path = request_line.next().unwrap_or_default();
    if !path.starts_with('/') {
        return Err(bad_request("request line has no absolute path"));
    }

    let mut content_length: Option<usize> = None;
    let mut content_type = ContentKind::Other;
    for line in head.split("\r\n").skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(
                value
                    .parse()
                    .map_err(|_| bad_request("Content-Length is not a number"))?,
            );
        } else if name.eq_ignore_ascii_case("content-type") {
            // A parameter such as `; charset=utf-8` does not change the type.
            content_type = match value.split(';').next().unwrap_or_default().trim() {
                "application/json" => ContentKind::Json,
                "text/plain" => ContentKind::Text,
                _ => ContentKind::Other,
            };
        }
    }

    let content_length = content_length.unwrap_or(0);
    if content_length > max_body {
        return Err(payload_too_large(max_body));
    }

    let body_start = boundary + 4;
    if raw.len() - body_start < content_length {
        return Err(bad_request("body is shorter than Content-Length"));
    }
    let body = raw[body_start..body_start + content_length].to_vec();

    if !body.is_empty() {
        let text = std::str::from_utf8(&body).map_err(|_| invalid_utf8())?;
        if content_type == ContentKind::Json {
            // Validated here so a bad body is refused before routing. `route`
            // parses it once more to extract the text; both parses are bounded
            // by `max_body` and both happen off the render path.
            json_text(text).map(|_| ())?;
        }
    }

    Ok(Request { method, path: path.to_string(), body, content_type })
}

/// Maps a parsed request onto the action the window thread should apply.
pub fn route(request: &Request) -> Result<Action, HttpError> {
    match (request.method, request.path.as_str()) {
        (Method::Post, "/text") => Ok(Action::SetText(body_text(request)?)),
        (Method::Post, "/time") => Ok(Action::SetTime),
        (Method::Post, "/quit") => Ok(Action::Quit),
        (Method::Get, "/status") => Ok(Action::Status),
        (_, path) if is_documented(path) => Err(method_not_allowed()),
        _ => Err(unknown_endpoint(&request.path)),
    }
}

fn is_documented(path: &str) -> bool {
    matches!(path, "/text" | "/time" | "/quit" | "/status")
}

/// The text a `/text` body carries: the body itself, or the `text` field of a
/// JSON body.
fn body_text(request: &Request) -> Result<String, HttpError> {
    let text = std::str::from_utf8(&request.body).map_err(|_| invalid_utf8())?;
    match request.content_type {
        ContentKind::Json => json_text(text),
        ContentKind::Text | ContentKind::Other => Ok(text.to_string()),
    }
}

fn json_text(body: &str) -> Result<String, HttpError> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|error| invalid_json(&error.to_string()))?;
    value
        .get("text")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid_json("expected an object with a string \"text\" field"))
}

/// The success envelope. `Action::Status` carries the whole snapshot; the rest
/// report the mode they left the window in.
pub fn success_body(action: &Action, snapshot: &StatusSnapshot) -> String {
    match action {
        Action::Status => {
            // Serialising primitive fields cannot fail; if it ever did, the
            // response would carry `ok` alone rather than panic in a handler.
            let fields = serde_json::to_value(snapshot).unwrap_or(serde_json::Value::Null);
            let mut object = serde_json::Map::new();
            object.insert("ok".to_string(), serde_json::Value::Bool(true));
            if let serde_json::Value::Object(fields) = fields {
                object.extend(fields);
            }
            serde_json::Value::Object(object).to_string()
        }
        Action::Quit => "{\"ok\":true}".to_string(),
        Action::SetText(_) | Action::SetTime => {
            format!("{{\"ok\":true,\"mode\":\"{}\"}}", mode_name(snapshot.mode))
        }
    }
}

pub fn error_body(error: &HttpError) -> String {
    serde_json::json!({
        "ok": false,
        "error": { "code": error.code, "message": error.message },
    })
    .to_string()
}

/// A request that is malformed in a way the spec's table does not name — no
/// body separator, a request line without a path, a bad `Content-Length`.
fn bad_request(message: &str) -> HttpError {
    HttpError { status: 400, code: "bad_request", message: message.to_string() }
}

fn invalid_json(message: &str) -> HttpError {
    HttpError { status: 400, code: "invalid_json", message: message.to_string() }
}

fn invalid_utf8() -> HttpError {
    HttpError {
        status: 400,
        code: "invalid_utf8",
        message: "body is not valid UTF-8".to_string(),
    }
}

fn method_not_allowed() -> HttpError {
    HttpError {
        status: 405,
        code: "method_not_allowed",
        message: "only POST /text, POST /time, POST /quit and GET /status are served".to_string(),
    }
}

fn unknown_endpoint(path: &str) -> HttpError {
    HttpError {
        status: 404,
        code: "unknown_endpoint",
        message: format!("no endpoint at {path:?}"),
    }
}

fn payload_too_large(max_body: usize) -> HttpError {
    HttpError {
        status: 413,
        code: "payload_too_large",
        message: format!("body exceeds {max_body} bytes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;

    const MAX: usize = 65536;

    fn post(path: &str, ctype: &str, body: &str) -> Vec<u8> {
        format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    #[test]
    fn text_plain_body_becomes_the_text() {
        let raw = post("/text", "text/plain", "玻璃纸");
        let req = parse_request(&raw, MAX).unwrap();
        assert_eq!(req.method, Method::Post);
        assert_eq!(route(&req).unwrap(), Action::SetText("玻璃纸".into()));
    }

    #[test]
    fn json_body_becomes_the_text() {
        let raw = post("/text", "application/json", r#"{"text":"hello"}"#);
        let req = parse_request(&raw, MAX).unwrap();
        assert_eq!(route(&req).unwrap(), Action::SetText("hello".into()));
    }

    #[test]
    fn empty_body_becomes_an_empty_text_which_is_blank_mode() {
        let raw = post("/text", "text/plain", "");
        let req = parse_request(&raw, MAX).unwrap();
        assert_eq!(route(&req).unwrap(), Action::SetText(String::new()));
    }

    #[test]
    fn body_limit_is_inclusive_at_65536_and_rejected_at_65537() {
        let ok = post("/text", "text/plain", &"x".repeat(MAX));
        assert!(parse_request(&ok, MAX).is_ok());

        let too_big = post("/text", "text/plain", &"x".repeat(MAX + 1));
        let err = parse_request(&too_big, MAX).unwrap_err();
        assert_eq!((err.status, err.code), (413, "payload_too_large"));

        // Declared-but-not-sent oversize is rejected before reading a body.
        let liar = b"POST /text HTTP/1.1\r\nContent-Length: 999999\r\n\r\n";
        let err = parse_request(liar, MAX).unwrap_err();
        assert_eq!((err.status, err.code), (413, "payload_too_large"));
    }

    #[test]
    fn malformed_and_unknown_requests_get_the_spec_codes() {
        let json = post("/text", "application/json", "{oops");
        assert_eq!(parse_request(&json, MAX).unwrap_err().code, "invalid_json");

        let bad_utf8 = {
            let mut raw = b"POST /text HTTP/1.1\r\nContent-Length: 2\r\n\r\n".to_vec();
            raw.extend_from_slice(&[0xff, 0xfe]);
            raw
        };
        assert_eq!(parse_request(&bad_utf8, MAX).unwrap_err().code, "invalid_utf8");

        let unknown = parse_request(b"GET /nope HTTP/1.1\r\n\r\n", MAX).unwrap();
        let err = route(&unknown).unwrap_err();
        assert_eq!((err.status, err.code), (404, "unknown_endpoint"));

        let wrong_method = parse_request(b"GET /text HTTP/1.1\r\n\r\n", MAX).unwrap();
        let err = route(&wrong_method).unwrap_err();
        assert_eq!((err.status, err.code), (405, "method_not_allowed"));
    }

    #[test]
    fn time_quit_and_status_are_routed() {
        assert_eq!(
            route(&parse_request(b"POST /time HTTP/1.1\r\n\r\n", MAX).unwrap()).unwrap(),
            Action::SetTime
        );
        assert_eq!(
            route(&parse_request(b"POST /quit HTTP/1.1\r\n\r\n", MAX).unwrap()).unwrap(),
            Action::Quit
        );
        assert_eq!(
            route(&parse_request(b"GET /status HTTP/1.1\r\n\r\n", MAX).unwrap()).unwrap(),
            Action::Status
        );
    }

    #[test]
    fn bodies_are_taken_as_raw_bytes_not_re_encoded() {
        // A multi-byte character split across the header/body boundary must survive.
        let body = "é".repeat(3);
        let raw = post("/text", "text/plain", &body);
        assert_eq!(route(&parse_request(&raw, MAX).unwrap()).unwrap(), Action::SetText(body));
    }

    #[test]
    fn status_body_carries_every_documented_field() {
        let snapshot = crate::status::StatusSnapshot {
            mode: crate::content::Mode::Text,
            window: Rect { x: 120, y: 120, width: 640, height: 220 },
            monitor_name: "\\\\.\\DISPLAY1".into(),
            dpi: 96,
            truncated: false,
            text_bytes: 42,
            position_source: crate::window_state::PositionSource::Config,
        };
        let json: serde_json::Value =
            serde_json::from_str(&success_body(&Action::Status, &snapshot)).unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["mode"], "text");
        assert_eq!(json["window"]["width"], 640);
        assert_eq!(json["monitor"]["name"], "\\\\.\\DISPLAY1");
        assert_eq!(json["monitor"]["dpi"], 96);
        assert_eq!(json["truncated"], false);
        assert_eq!(json["text_bytes"], 42);
        assert_eq!(json["position_source"], "config");
    }

    #[test]
    fn error_body_matches_the_envelope_shape() {
        let json: serde_json::Value = serde_json::from_str(&error_body(&HttpError {
            status: 413,
            code: "payload_too_large",
            message: "body exceeds 65536 bytes".into(),
        }))
        .unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"]["code"], "payload_too_large");
        assert!(json["error"]["message"].as_str().unwrap().contains("65536"));
    }
}
