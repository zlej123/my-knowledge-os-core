use std::{
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use mko_core::{
    config_v2::PerspectiveV2,
    error::MkoError,
    home::{HomeNextAction, HomeReport, inspect_home},
    json_v2::{QueueItemStateV2, QueueItemTypeV2, QueueNextActionV2},
    provider_scan::MonotonicElapsedClock,
    queue_v2::{
        KnowledgeSearchLayerV2, ResurfacedKnowledgeStateV2, derive_queue_v2,
        resurface_knowledge_by_perspective_v2, search_approved_knowledge_by_perspective_v2,
    },
    quick_note_v2::search_quick_notes_v2,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const HTML: &str = include_str!("ui.html");
const MAX_REQUEST_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_UI_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SEARCH_CHARS: usize = 200;
const MAX_THESES: usize = 20;
const IO_TIMEOUT: Duration = Duration::from_millis(900);
const UI_REQUEST_DEADLINE: Duration = Duration::from_secs(2);
const THESIS_REQUEST_DEADLINE: Duration = Duration::from_secs(2);

pub struct UiConfig {
    pub repository_root: PathBuf,
    pub provider_root: PathBuf,
    pub bind: String,
    pub thesis_url: String,
    pub thesis_token: Option<String>,
    pub max_requests: Option<usize>,
}

struct UiState {
    repository_root: PathBuf,
    provider_root: PathBuf,
    thesis: ThesisReadClient,
    allowed_hosts: Vec<String>,
}

pub fn serve_ui(config: UiConfig) -> Result<(), MkoError> {
    let bind = parse_loopback_bind(&config.bind)?;
    if config.max_requests == Some(0) {
        return Err(MkoError::new(
            "ui_request_limit_invalid",
            "mko ui max_requests must be greater than zero",
        ));
    }
    let thesis_endpoint = LocalHttpEndpoint::parse(&config.thesis_url)?;
    let thesis = ThesisReadClient::new(thesis_endpoint, config.thesis_token)?;
    let listener = TcpListener::bind(bind)
        .map_err(|error| MkoError::new("ui_bind_failed", error.to_string()))?;
    let local = listener
        .local_addr()
        .map_err(|error| MkoError::new("ui_bind_failed", error.to_string()))?;
    let state = UiState {
        repository_root: config.repository_root,
        provider_root: config.provider_root,
        thesis,
        allowed_hosts: allowed_hosts(local),
    };
    println!("MKO 통합 UI: http://{local}/");
    println!("읽기 전용입니다. 승인·발행·실행 endpoint는 제공하지 않습니다.");
    let mut served = 0usize;
    for stream in listener.incoming() {
        let mut stream =
            stream.map_err(|error| MkoError::new("ui_accept_failed", error.to_string()))?;
        if let Err(error) = handle_connection(&mut stream, &state) {
            let response = HttpResponse::json(
                500,
                serde_json::json!({"error":"ui_request_failed","detail":error.message()}),
            );
            let _ = write_response(&mut stream, &response, false);
        }
        served = served.saturating_add(1);
        if config.max_requests.is_some_and(|limit| served >= limit) {
            break;
        }
    }
    Ok(())
}

fn parse_loopback_bind(value: &str) -> Result<SocketAddr, MkoError> {
    let address = value.parse::<SocketAddr>().map_err(|_| {
        MkoError::new(
            "ui_bind_invalid",
            "mko ui --bind must be an IP address and port such as 127.0.0.1:2036",
        )
    })?;
    if !address.ip().is_loopback() {
        return Err(MkoError::new(
            "ui_bind_not_loopback",
            "mko ui only binds to a loopback address",
        ));
    }
    Ok(address)
}

fn allowed_hosts(address: SocketAddr) -> Vec<String> {
    let port = address.port();
    let mut hosts = vec![format!("localhost:{port}")];
    match address.ip() {
        IpAddr::V4(ip) => hosts.push(format!("{ip}:{port}")),
        IpAddr::V6(ip) => hosts.push(format!("[{ip}]:{port}")),
    }
    hosts
}

fn handle_connection(stream: &mut TcpStream, state: &UiState) -> Result<(), MkoError> {
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|error| MkoError::new("ui_request_failed", error.to_string()))?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|error| MkoError::new("ui_request_failed", error.to_string()))?;
    let request = match read_request(stream) {
        Ok(request) => request,
        Err(error) => {
            let response = HttpResponse::json(
                400,
                serde_json::json!({"error":error.code(),"detail":"HTTP 요청 형식이 올바르지 않습니다."}),
            );
            return write_response(stream, &response, false);
        }
    };
    let head_only = request.method == "HEAD";
    let response = if !state
        .allowed_hosts
        .iter()
        .any(|allowed| request.host.eq_ignore_ascii_case(allowed))
    {
        HttpResponse::text(421, "요청한 Host로는 이 로컬 UI를 열 수 없습니다.")
    } else {
        route_request(&request, state)
    };
    write_response(stream, &response, head_only)
}

struct IncomingRequest {
    method: String,
    target: String,
    host: String,
}

fn read_request(stream: &mut TcpStream) -> Result<IncomingRequest, MkoError> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 1024];
    let deadline = Instant::now() + UI_REQUEST_DEADLINE;
    let header_end = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(MkoError::new(
                "ui_request_invalid",
                "HTTP request deadline exceeded",
            ));
        }
        stream
            .set_read_timeout(Some(remaining.min(IO_TIMEOUT)))
            .map_err(|error| MkoError::new("ui_request_invalid", error.to_string()))?;
        let read = stream
            .read(&mut chunk)
            .map_err(|error| MkoError::new("ui_request_invalid", error.to_string()))?;
        if read == 0 {
            return Err(MkoError::new(
                "ui_request_invalid",
                "HTTP request headers are incomplete",
            ));
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(MkoError::new(
                "ui_request_invalid",
                "HTTP request headers are too large",
            ));
        }
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
    };
    let text = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| MkoError::new("ui_request_invalid", "HTTP request must be UTF-8"))?;
    let mut lines = text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| MkoError::new("ui_request_invalid", "HTTP request line is missing"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let version = parts.next().unwrap_or_default();
    let valid_method = !method.is_empty()
        && method.len() <= 32
        && method
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte == b'-');
    let valid_target = if matches!(method.as_str(), "GET" | "HEAD") {
        target.starts_with('/')
    } else {
        !target.is_empty()
    };
    if parts.next().is_some() || !valid_method || !valid_target || version != "HTTP/1.1" {
        return Err(MkoError::new(
            "ui_request_invalid",
            "HTTP request line is invalid",
        ));
    }
    let mut host = None;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| MkoError::new("ui_request_invalid", "HTTP header is invalid"))?;
        if name.eq_ignore_ascii_case("host") {
            if host.is_some() {
                return Err(MkoError::new(
                    "ui_request_invalid",
                    "HTTP request must contain exactly one Host header",
                ));
            }
            let value = value.trim();
            if value.is_empty() {
                return Err(MkoError::new(
                    "ui_request_invalid",
                    "Host header must not be empty",
                ));
            }
            host = Some(value.to_owned());
        }
    }
    let host =
        host.ok_or_else(|| MkoError::new("ui_request_invalid", "Host header is required"))?;
    Ok(IncomingRequest {
        method,
        target,
        host,
    })
}

fn route_request(request: &IncomingRequest, state: &UiState) -> HttpResponse {
    if !matches!(request.method.as_str(), "GET" | "HEAD") {
        return HttpResponse::json(
            405,
            serde_json::json!({
                "error":"read_only",
                "detail":"MKO 통합 UI는 읽기 전용이며 mutation endpoint를 제공하지 않습니다."
            }),
        )
        .with_header("Allow", "GET, HEAD");
    }
    let (path, query) = request
        .target
        .split_once('?')
        .map_or((request.target.as_str(), ""), |(path, query)| (path, query));
    match path {
        "/" => HttpResponse::html(200, HTML.as_bytes().to_vec()),
        "/health" => HttpResponse::json(
            200,
            serde_json::json!({"status":"ok","service":"mko-ui","mode":"read_only"}),
        ),
        "/api/projection" => match build_projection(state) {
            Ok(projection) => HttpResponse::json(200, projection),
            Err(error) => HttpResponse::json(
                500,
                serde_json::json!({"error":error.code(),"detail":error.message()}),
            ),
        },
        "/api/search" => match search_projection(&state.repository_root, query) {
            Ok(results) => HttpResponse::json(200, serde_json::json!({"items":results})),
            Err(error) => {
                let status = if error.code() == "ui_search_invalid" {
                    400
                } else {
                    500
                };
                HttpResponse::json(
                    status,
                    serde_json::json!({"error":error.code(),"detail":error.message()}),
                )
            }
        },
        _ => HttpResponse::json(
            404,
            serde_json::json!({"error":"not_found","detail":"route not found"}),
        ),
    }
}

struct HttpResponse {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    headers: Vec<(&'static str, &'static str)>,
}

impl HttpResponse {
    fn html(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: "text/html; charset=utf-8",
            body,
            headers: Vec::new(),
        }
    }

    fn text(status: u16, text: &str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: text.as_bytes().to_vec(),
            headers: Vec::new(),
        }
    }

    fn json(status: u16, value: impl Serialize) -> Self {
        let (status, body) = match serde_json::to_vec(&value) {
            Ok(body) if body.len() <= MAX_UI_RESPONSE_BYTES => (status, body),
            Ok(_) => (
                500,
                br#"{"error":"response_too_large","detail":"response exceeds the local UI limit"}"#
                    .to_vec(),
            ),
            Err(_) => (
                500,
                br#"{"error":"serialization_failed","detail":"response serialization failed"}"#
                    .to_vec(),
            ),
        };
        Self {
            status,
            content_type: "application/json; charset=utf-8",
            body,
            headers: Vec::new(),
        }
    }

    fn with_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }
}

fn write_response(
    stream: &mut TcpStream,
    response: &HttpResponse,
    head_only: bool,
) -> Result<(), MkoError> {
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        421 => "Misdirected Request",
        _ => "Internal Server Error",
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n",
        response.status,
        reason,
        response.content_type,
        response.body.len()
    );
    for (name, value) in &response.headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .and_then(|()| {
            if head_only {
                Ok(())
            } else {
                stream.write_all(&response.body)
            }
        })
        .map_err(|error| MkoError::new("ui_response_failed", error.to_string()))
}

#[derive(Serialize)]
struct UiProjection {
    schema_version: u32,
    mode: &'static str,
    generated_at_unix: u64,
    core: CoreProjection,
    investment: InvestmentProjection,
    attention: Vec<AttentionItem>,
}

#[derive(Serialize)]
struct CoreProjection {
    generation: &'static str,
    next_action: &'static str,
    counts: CoreCounts,
    scan_complete: bool,
    queue: Vec<CoreQueueItem>,
    stuck: Vec<CoreStuckItem>,
    knowledge: Vec<CoreKnowledgeItem>,
}

#[derive(Default, Serialize)]
struct CoreCounts {
    new_material: u64,
    in_progress: u64,
    review_pending: u64,
    changes_requested: u64,
    approved_knowledge: u64,
    blocked: u64,
}

#[derive(Serialize)]
struct CoreQueueItem {
    item_id: String,
    title: String,
    item_type: &'static str,
    state: &'static str,
    revisions: Vec<String>,
    next_action: &'static str,
    command: String,
}

#[derive(Serialize)]
struct CoreStuckItem {
    asset_id: String,
    title: String,
    reason: &'static str,
    next_action: &'static str,
}

#[derive(Serialize)]
struct CoreKnowledgeItem {
    knowledge_id: String,
    current_revision: String,
    title: String,
    synthesis: String,
    perspectives: Vec<String>,
    review_state: &'static str,
    reviewed_at: String,
    has_open_questions: bool,
}

#[derive(Serialize)]
struct SearchResult {
    record_type: &'static str,
    record_id: String,
    title: String,
    body: String,
    revision: Option<String>,
    layer: &'static str,
    perspectives: Vec<String>,
    locators: Vec<String>,
}

#[derive(Serialize)]
struct AttentionItem {
    domain: &'static str,
    kind: &'static str,
    title: String,
    detail: String,
    next_action: String,
    command: Option<String>,
    priority: u8,
}

fn build_projection(state: &UiState) -> Result<UiProjection, MkoError> {
    let core = build_core_projection(&state.repository_root, &state.provider_root)?;
    let investment = state.thesis.read_projection();
    let mut attention = core_attention(&core);
    attention.extend(investment_attention(&investment));
    attention.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then(left.kind.cmp(right.kind))
            .then(left.title.cmp(&right.title))
    });
    Ok(UiProjection {
        schema_version: 1,
        mode: "read_only",
        generated_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        core,
        investment,
        attention,
    })
}

fn build_core_projection(repository: &Path, provider: &Path) -> Result<CoreProjection, MkoError> {
    let report = inspect_home(repository, provider, &MonotonicElapsedClock::start())?;
    match report {
        HomeReport::Legacy(report) => Ok(CoreProjection {
            generation: "legacy_v1",
            next_action: home_action_label(HomeReport::Legacy(report.clone()).next_action()),
            counts: CoreCounts {
                new_material: report.new_material,
                in_progress: report.registered.saturating_add(report.incomplete),
                review_pending: report.review_pending,
                changes_requested: 0,
                approved_knowledge: report.complete,
                blocked: report.blocked,
            },
            scan_complete: true,
            queue: Vec::new(),
            stuck: Vec::new(),
            knowledge: Vec::new(),
        }),
        HomeReport::V3(report) => {
            let queue = derive_queue_v2(repository)?
                .items
                .into_iter()
                .map(|item| {
                    let next_action = queue_action_label(&item.next_action);
                    let command = match item.next_action {
                        QueueNextActionV2::Diagnose => "mko doctor".to_owned(),
                        QueueNextActionV2::Display | QueueNextActionV2::Regenerate => {
                            format!("mko show {}", item.item_id)
                        }
                    };
                    CoreQueueItem {
                        item_id: item.item_id,
                        title: item.title,
                        item_type: queue_type_label(&item.item_type),
                        state: queue_state_label(&item.state),
                        revisions: item.revisions,
                        next_action,
                        command,
                    }
                })
                .collect();
            let knowledge = resurface_knowledge_by_perspective_v2(repository, None, 24)?
                .into_iter()
                .map(|item| CoreKnowledgeItem {
                    knowledge_id: item.knowledge_id,
                    current_revision: item.current_revision,
                    title: item.title,
                    synthesis: item.synthesis,
                    perspectives: item
                        .perspectives
                        .iter()
                        .map(|perspective| perspective.as_str().to_owned())
                        .collect(),
                    review_state: match item.review_state {
                        ResurfacedKnowledgeStateV2::Approved => "approved",
                        ResurfacedKnowledgeStateV2::Deferred => "deferred",
                    },
                    reviewed_at: item.reviewed_at.to_rfc3339(),
                    has_open_questions: item.has_open_questions,
                })
                .collect();
            let stuck = report
                .stuck
                .iter()
                .map(|item| {
                    let (reason, next_action) = match item.reason {
                        mko_core::attempt_v2::StuckReasonV2::NotAttempted => {
                            ("not_attempted", "자료 정리 계속")
                        }
                        mko_core::attempt_v2::StuckReasonV2::TextUnreadable => {
                            ("text_unreadable", "내보내거나 스캔한 새 사본 등록")
                        }
                        mko_core::attempt_v2::StuckReasonV2::DownloadRequired => {
                            ("download_required", "원본을 내려받고 다시 시도")
                        }
                        mko_core::attempt_v2::StuckReasonV2::Retryable => {
                            ("retryable", "자료 정리 다시 시도")
                        }
                    };
                    CoreStuckItem {
                        asset_id: item.asset_id.clone(),
                        title: item.title.clone(),
                        reason,
                        next_action,
                    }
                })
                .collect();
            Ok(CoreProjection {
                generation: "v3",
                next_action: home_action_label(HomeReport::V3(report.clone()).next_action()),
                counts: CoreCounts {
                    new_material: report.new_material,
                    in_progress: report.in_progress,
                    review_pending: report.review_pending,
                    changes_requested: report.changes_requested,
                    approved_knowledge: report.approved_knowledge,
                    blocked: report.blocked,
                },
                scan_complete: report.scan_complete,
                queue,
                stuck,
                knowledge,
            })
        }
    }
}

fn home_action_label(action: HomeNextAction) -> &'static str {
    match action {
        HomeNextAction::Add => "자료 정리",
        HomeNextAction::Review => "검토 계속",
        HomeNextAction::Repair => "문제 확인",
        HomeNextAction::None => "지식 찾기",
    }
}

fn queue_type_label(value: &QueueItemTypeV2) -> &'static str {
    match value {
        QueueItemTypeV2::Source => "source",
        QueueItemTypeV2::Knowledge => "knowledge",
        QueueItemTypeV2::Combined => "combined",
    }
}

fn queue_state_label(value: &QueueItemStateV2) -> &'static str {
    match value {
        QueueItemStateV2::Unreviewed => "unreviewed",
        QueueItemStateV2::Deferred => "deferred",
        QueueItemStateV2::ChangesRequested => "changes_requested",
        QueueItemStateV2::RevisedUnreviewed => "revised_unreviewed",
        QueueItemStateV2::Blocked => "blocked",
    }
}

fn queue_action_label(value: &QueueNextActionV2) -> &'static str {
    match value {
        QueueNextActionV2::Display => "지식 검토",
        QueueNextActionV2::Regenerate => "수정본 준비",
        QueueNextActionV2::Diagnose => "문제 확인",
    }
}

fn core_attention(core: &CoreProjection) -> Vec<AttentionItem> {
    let mut items = core
        .queue
        .iter()
        .map(|item| AttentionItem {
            domain: "knowledge",
            kind: item.item_type,
            title: item.title.clone(),
            detail: format!("{} · revision {}건", item.state, item.revisions.len()),
            next_action: item.next_action.to_owned(),
            command: Some(item.command.clone()),
            priority: if item.state == "blocked" { 0 } else { 3 },
        })
        .collect::<Vec<_>>();
    items.extend(core.stuck.iter().map(|item| AttentionItem {
        domain: "source",
        kind: "recovery",
        title: item.title.clone(),
        detail: item.reason.to_owned(),
        next_action: item.next_action.to_owned(),
        command: Some("mko add --inbox".to_owned()),
        priority: 0,
    }));
    items
}

fn search_projection(repository: &Path, query: &str) -> Result<Vec<SearchResult>, MkoError> {
    let parameters = parse_query(query)?;
    let term = parameters
        .iter()
        .find(|(key, _)| key == "q")
        .map(|(_, value)| value.trim())
        .unwrap_or_default();
    if term.is_empty() || term.chars().count() > MAX_SEARCH_CHARS {
        return Err(MkoError::new(
            "ui_search_invalid",
            "검색어는 1자 이상 200자 이하여야 합니다",
        ));
    }
    let perspective_value = parameters
        .iter()
        .find(|(key, _)| key == "perspective")
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty());
    let perspective = match perspective_value {
        Some("life") => Some(PerspectiveV2::Life),
        Some("learning") => Some(PerspectiveV2::Learning),
        Some("technical") => Some(PerspectiveV2::Technical),
        Some("project") => Some(PerspectiveV2::Project),
        Some("investment") => Some(PerspectiveV2::Investment),
        Some(_) => {
            return Err(MkoError::new(
                "ui_search_invalid",
                "지원하지 않는 관점 필터입니다",
            ));
        }
        None => None,
    };
    let mut results = search_approved_knowledge_by_perspective_v2(repository, term, perspective)?
        .into_iter()
        .map(|item| SearchResult {
            record_type: "knowledge",
            record_id: item.knowledge_id,
            title: item.title,
            body: item.body,
            revision: Some(item.current_revision),
            layer: match item.layer {
                KnowledgeSearchLayerV2::GroundedEvidence => "grounded_evidence",
                KnowledgeSearchLayerV2::LlmAnalysis => "llm_analysis",
                KnowledgeSearchLayerV2::CounterargumentOrUncertainty => {
                    "counterargument_or_uncertainty"
                }
            },
            perspectives: item
                .perspectives
                .iter()
                .map(|perspective| perspective.as_str().to_owned())
                .collect(),
            locators: item.locators,
        })
        .collect::<Vec<_>>();
    if perspective.is_none() {
        results.extend(
            search_quick_notes_v2(repository, term)?
                .into_iter()
                .map(|note| SearchResult {
                    record_type: "quick_note",
                    record_id: note.id,
                    title: "내 생각".to_owned(),
                    body: note.text,
                    revision: Some(note.text_digest),
                    layer: "user_thought",
                    perspectives: Vec::new(),
                    locators: Vec::new(),
                }),
        );
    }
    results.sort_by(|left, right| {
        left.record_type
            .cmp(right.record_type)
            .then(left.title.cmp(&right.title))
            .then(left.record_id.cmp(&right.record_id))
    });
    Ok(results)
}

fn parse_query(query: &str) -> Result<Vec<(String, String)>, MkoError> {
    if query.len() > 2048 {
        return Err(MkoError::new("ui_search_invalid", "query is too large"));
    }
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            Ok((percent_decode(key)?, percent_decode(value)?))
        })
        .collect()
}

fn percent_decode(value: &str) -> Result<String, MkoError> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let high = hex_value(bytes[index + 1]);
                let low = hex_value(bytes[index + 2]);
                let (Some(high), Some(low)) = (high, low) else {
                    return Err(MkoError::new(
                        "ui_search_invalid",
                        "query encoding is invalid",
                    ));
                };
                output.push((high << 4) | low);
                index += 3;
            }
            b'%' => {
                return Err(MkoError::new(
                    "ui_search_invalid",
                    "query encoding is invalid",
                ));
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(output)
        .map_err(|_| MkoError::new("ui_search_invalid", "query must be valid UTF-8"))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Default, Serialize)]
struct InvestmentProjection {
    status: &'static str,
    message: String,
    next_action: Option<String>,
    source: &'static str,
    pending: InvestmentCounts,
    filing_candidates: Vec<InvestmentFiling>,
    evidence: Vec<InvestmentEvidence>,
    decisions: Vec<InvestmentDecision>,
    executions: Vec<InvestmentExecution>,
    theses: Vec<InvestmentThesis>,
    warnings: Vec<String>,
}

#[derive(Default, Serialize)]
struct InvestmentCounts {
    filings: usize,
    evidence: usize,
    decisions: usize,
    executions: usize,
}

#[derive(Serialize)]
struct InvestmentFiling {
    id: String,
    company_id: String,
    form: String,
    filing_date: String,
    filing_uri: String,
}

#[derive(Serialize)]
struct InvestmentEvidence {
    id: String,
    company_id: String,
    claim_key: String,
    relation: String,
    summary: String,
    locator: String,
    source_title: String,
    source_uri: String,
}

#[derive(Serialize)]
struct InvestmentDecision {
    id: String,
    portfolio_id: String,
    rationale: String,
    cash_weight: f64,
    position_count: usize,
    created_at: String,
}

#[derive(Serialize)]
struct InvestmentExecution {
    id: String,
    portfolio_decision_id: String,
    proposal_count: usize,
    notional: f64,
    state: &'static str,
    created_at: String,
}

#[derive(Serialize)]
struct InvestmentThesis {
    thesis_id: String,
    company_id: String,
    version: u32,
    core_claim: String,
    claims: Vec<ThesisClaimPayload>,
    falsification_conditions: Vec<FalsificationPayload>,
    publication_state: &'static str,
    publication_version: Option<u32>,
    publication_status: Option<String>,
    published_at: Option<String>,
    evidence_count: usize,
}

fn investment_attention(investment: &InvestmentProjection) -> Vec<AttentionItem> {
    let mut items = investment
        .executions
        .iter()
        .map(|item| AttentionItem {
            domain: "investment",
            kind: "execution_plan",
            title: item.id.clone(),
            detail: format!("{} · 주문 {}건", item.state, item.proposal_count),
            next_action: if item.state == "ready_paper_execution" {
                "기존 Thesis 승인함에서 가상 체결 검토".to_owned()
            } else {
                "기존 Thesis 승인함에서 실행 계획 검토".to_owned()
            },
            command: None,
            priority: 1,
        })
        .collect::<Vec<_>>();
    items.extend(investment.decisions.iter().map(|item| AttentionItem {
        domain: "investment",
        kind: "portfolio_decision",
        title: item.id.clone(),
        detail: item.rationale.clone(),
        next_action: "기존 Thesis 승인함에서 포트폴리오 판단 검토".to_owned(),
        command: None,
        priority: 1,
    }));
    items.extend(investment.evidence.iter().map(|item| AttentionItem {
        domain: "investment",
        kind: "evidence_relation",
        title: format!("{} · {}", item.company_id, item.claim_key),
        detail: format!("{} · {}", item.relation, item.summary),
        next_action: "기존 Thesis 승인함에서 근거 관계 검토".to_owned(),
        command: None,
        priority: 2,
    }));
    items.extend(
        investment
            .filing_candidates
            .iter()
            .map(|item| AttentionItem {
                domain: "investment",
                kind: "filing_candidate",
                title: format!("{} · {}", item.company_id, item.form),
                detail: format!("{} 제출", item.filing_date),
                next_action: "공시 원문을 읽고 Evidence 후보 여부 판단".to_owned(),
                command: None,
                priority: 4,
            }),
    );
    items
}

#[derive(Clone, Debug)]
struct LocalHttpEndpoint {
    connect_host: String,
    host_header: String,
    port: u16,
    base_path: String,
}

impl LocalHttpEndpoint {
    fn parse(value: &str) -> Result<Self, MkoError> {
        let rest = value.strip_prefix("http://").ok_or_else(|| {
            MkoError::new(
                "ui_thesis_url_invalid",
                "mko ui only reads a local http:// Thesis endpoint",
            )
        })?;
        if rest.contains('@') || rest.contains('#') || rest.contains('?') {
            return Err(MkoError::new(
                "ui_thesis_url_invalid",
                "Thesis endpoint must not contain credentials, a query, or a fragment",
            ));
        }
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        if !path.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~' | b'%')
        }) {
            return Err(MkoError::new(
                "ui_thesis_url_invalid",
                "Thesis endpoint path contains unsupported characters",
            ));
        }
        let base_path = if path.is_empty() {
            String::new()
        } else {
            format!("/{}", path.trim_end_matches('/'))
        };
        let (connect_host, host_header, port) = if let Some(authority) = authority.strip_prefix('[')
        {
            let (host, port) = authority.split_once("]:").ok_or_else(|| {
                MkoError::new("ui_thesis_url_invalid", "IPv6 Thesis endpoint is invalid")
            })?;
            let address = host.parse::<IpAddr>().map_err(|_| {
                MkoError::new("ui_thesis_url_invalid", "Thesis endpoint host is invalid")
            })?;
            if !address.is_loopback() {
                return Err(MkoError::new(
                    "ui_thesis_url_not_loopback",
                    "mko ui only reads Thesis from loopback",
                ));
            }
            let port = port.parse::<u16>().map_err(|_| {
                MkoError::new("ui_thesis_url_invalid", "Thesis endpoint port is invalid")
            })?;
            (host.to_owned(), format!("[{host}]:{port}"), port)
        } else {
            let (host, port) = authority.rsplit_once(':').ok_or_else(|| {
                MkoError::new(
                    "ui_thesis_url_invalid",
                    "Thesis endpoint must include a port",
                )
            })?;
            if host != "localhost"
                && host
                    .parse::<IpAddr>()
                    .map_or(true, |address| !address.is_loopback())
            {
                return Err(MkoError::new(
                    "ui_thesis_url_not_loopback",
                    "mko ui only reads Thesis from loopback",
                ));
            }
            let port = port.parse::<u16>().map_err(|_| {
                MkoError::new("ui_thesis_url_invalid", "Thesis endpoint port is invalid")
            })?;
            let connect_host = if host == "localhost" {
                "127.0.0.1".to_owned()
            } else {
                host.to_owned()
            };
            (connect_host, format!("{host}:{port}"), port)
        };
        Ok(Self {
            connect_host,
            host_header,
            port,
            base_path,
        })
    }

    fn path(&self, path: &str) -> String {
        format!("{}{path}", self.base_path)
    }
}

struct ThesisReadClient {
    endpoint: LocalHttpEndpoint,
    token: Option<String>,
}

impl ThesisReadClient {
    fn new(endpoint: LocalHttpEndpoint, token: Option<String>) -> Result<Self, MkoError> {
        if token.as_ref().is_some_and(|value| {
            value.len() > 4096
                || value.is_empty()
                || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        }) {
            return Err(MkoError::new(
                "ui_thesis_token_invalid",
                "THESIS_API_TOKEN must be 1-4096 visible ASCII characters",
            ));
        }
        Ok(Self { endpoint, token })
    }

    fn read_projection(&self) -> InvestmentProjection {
        let inbox = match self.get::<ApprovalInboxPayload>("/v1/approval-inbox") {
            Ok(inbox) => inbox,
            Err(error) => return investment_unavailable(error),
        };
        let mut warnings = Vec::new();
        let theses = match self.get::<Vec<ThesisVersionPayload>>("/v1/theses") {
            Ok(theses) => theses,
            Err(error) => {
                warnings.push(error.user_message());
                Vec::new()
            }
        };
        let thesis_cards = theses
            .into_iter()
            .take(MAX_THESES)
            .map(|thesis| {
                let path = format!(
                    "/v1/theses/{}/publication",
                    encode_path_segment(&thesis.thesis_id)
                );
                let (publication, publication_state) =
                    match self.get_optional::<ThesisPublicationPayload>(&path) {
                        Ok(Some(publication)) => (Some(publication), "published"),
                        Ok(None) => (None, "not_published"),
                        Err(error) => {
                            warnings.push(error.user_message());
                            (None, "unknown")
                        }
                    };
                InvestmentThesis {
                    thesis_id: thesis.thesis_id,
                    company_id: thesis.company_id,
                    version: thesis.version,
                    core_claim: thesis.core_claim,
                    claims: thesis.claims,
                    falsification_conditions: thesis.falsification_conditions,
                    publication_state,
                    publication_version: publication.as_ref().map(|item| item.version),
                    publication_status: publication.as_ref().map(|item| item.status.clone()),
                    published_at: publication.as_ref().map(|item| item.published_at.clone()),
                    evidence_count: publication.map_or(0, |item| item.evidence_ids.len()),
                }
            })
            .collect::<Vec<_>>();
        let filing_candidates = inbox
            .pending_filing_candidates
            .into_iter()
            .map(|item| InvestmentFiling {
                id: item.id,
                company_id: item.company_id,
                form: item.form,
                filing_date: item.filing_date,
                filing_uri: item.filing_uri,
            })
            .collect::<Vec<_>>();
        let evidence = inbox
            .pending_evidence
            .into_iter()
            .map(|item| InvestmentEvidence {
                id: item.id,
                company_id: item.company_id,
                claim_key: item.claim_key,
                relation: item.relation,
                summary: item.summary,
                locator: item.locator,
                source_title: item.source.title,
                source_uri: item.source.uri,
            })
            .collect::<Vec<_>>();
        let decisions = inbox
            .pending_portfolio_decisions
            .into_iter()
            .map(|item| InvestmentDecision {
                id: item.id,
                portfolio_id: item.portfolio_id,
                rationale: item.rationale,
                cash_weight: item.target.cash_weight,
                position_count: item.target.positions.len(),
                created_at: item.created_at,
            })
            .collect::<Vec<_>>();
        let mut executions = inbox
            .pending_execution_plans
            .into_iter()
            .map(|item| investment_execution(item, "pending_review"))
            .collect::<Vec<_>>();
        executions.extend(
            inbox
                .ready_paper_execution_plans
                .into_iter()
                .map(|item| investment_execution(item, "ready_paper_execution")),
        );
        InvestmentProjection {
            status: if warnings.is_empty() {
                "connected"
            } else {
                "partial"
            },
            message: "기존 Thesis 상태를 읽기 전용으로 연결했습니다.".to_owned(),
            next_action: None,
            source: "transitional_local_adapter",
            pending: InvestmentCounts {
                filings: filing_candidates.len(),
                evidence: evidence.len(),
                decisions: decisions.len(),
                executions: executions.len(),
            },
            filing_candidates,
            evidence,
            decisions,
            executions,
            theses: thesis_cards,
            warnings,
        }
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ThesisReadError> {
        let (status, body) = self.get_bytes(path)?;
        if status == 401 {
            return Err(ThesisReadError::Unauthorized);
        }
        if !(200..300).contains(&status) {
            return Err(ThesisReadError::Status(status));
        }
        serde_json::from_slice(&body).map_err(|_| ThesisReadError::InvalidResponse)
    }

    fn get_optional<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>, ThesisReadError> {
        let (status, body) = self.get_bytes(path)?;
        if status == 404 {
            return Ok(None);
        }
        if status == 401 {
            return Err(ThesisReadError::Unauthorized);
        }
        if !(200..300).contains(&status) {
            return Err(ThesisReadError::Status(status));
        }
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|_| ThesisReadError::InvalidResponse)
    }

    fn get_bytes(&self, path: &str) -> Result<(u16, Vec<u8>), ThesisReadError> {
        let mut stream =
            TcpStream::connect((self.endpoint.connect_host.as_str(), self.endpoint.port))
                .map_err(|_| ThesisReadError::Unavailable)?;
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(|_| ThesisReadError::Unavailable)?;
        stream
            .set_write_timeout(Some(IO_TIMEOUT))
            .map_err(|_| ThesisReadError::Unavailable)?;
        let mut request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\n",
            self.endpoint.path(path),
            self.endpoint.host_header
        );
        if let Some(token) = &self.token {
            request.push_str("Authorization: Bearer ");
            request.push_str(token);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .map_err(|_| ThesisReadError::Unavailable)?;
        let deadline = Instant::now() + THESIS_REQUEST_DEADLINE;
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ThesisReadError::Unavailable);
            }
            stream
                .set_read_timeout(Some(remaining.min(IO_TIMEOUT)))
                .map_err(|_| ThesisReadError::Unavailable)?;
            let read = stream
                .read(&mut chunk)
                .map_err(|_| ThesisReadError::Unavailable)?;
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..read]);
            if bytes.len() > MAX_RESPONSE_BYTES {
                return Err(ThesisReadError::ResponseTooLarge);
            }
        }
        parse_http_response(&bytes)
    }
}

fn encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn parse_http_response(bytes: &[u8]) -> Result<(u16, Vec<u8>), ThesisReadError> {
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or(ThesisReadError::InvalidResponse)?;
    let header =
        std::str::from_utf8(&bytes[..header_end]).map_err(|_| ThesisReadError::InvalidResponse)?;
    let status = header
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or(ThesisReadError::InvalidResponse)?;
    let body = &bytes[header_end + 4..];
    let mut content_length = None;
    let mut chunked = false;
    for line in header.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            return Err(ThesisReadError::InvalidResponse);
        };
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(ThesisReadError::InvalidResponse);
            }
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| ThesisReadError::InvalidResponse)?,
            );
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.trim().eq_ignore_ascii_case("chunked") {
                return Err(ThesisReadError::InvalidResponse);
            }
            chunked = true;
        }
    }
    let body = if chunked {
        if content_length.is_some() {
            return Err(ThesisReadError::InvalidResponse);
        }
        decode_chunked(body)?
    } else {
        if content_length.is_some_and(|expected| expected != body.len()) {
            return Err(ThesisReadError::InvalidResponse);
        }
        body.to_vec()
    };
    Ok((status, body))
}

fn decode_chunked(bytes: &[u8]) -> Result<Vec<u8>, ThesisReadError> {
    let mut output = Vec::new();
    let mut remaining = bytes;
    loop {
        let line_end = remaining
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or(ThesisReadError::InvalidResponse)?;
        let size_text = std::str::from_utf8(&remaining[..line_end])
            .map_err(|_| ThesisReadError::InvalidResponse)?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or_default(), 16)
            .map_err(|_| ThesisReadError::InvalidResponse)?;
        remaining = &remaining[line_end + 2..];
        if size == 0 {
            break;
        }
        if remaining.len() < size + 2 || &remaining[size..size + 2] != b"\r\n" {
            return Err(ThesisReadError::InvalidResponse);
        }
        output.extend_from_slice(&remaining[..size]);
        if output.len() > MAX_RESPONSE_BYTES {
            return Err(ThesisReadError::ResponseTooLarge);
        }
        remaining = &remaining[size + 2..];
    }
    Ok(output)
}

#[derive(Clone, Copy, Debug)]
enum ThesisReadError {
    Unavailable,
    Unauthorized,
    InvalidResponse,
    ResponseTooLarge,
    Status(u16),
}

impl ThesisReadError {
    fn user_message(self) -> String {
        match self {
            Self::Unavailable => "투자 모듈에 연결할 수 없습니다.".to_owned(),
            Self::Unauthorized => "투자 모듈 토큰이 필요하거나 일치하지 않습니다.".to_owned(),
            Self::InvalidResponse => "투자 모듈 응답 계약이 맞지 않습니다.".to_owned(),
            Self::ResponseTooLarge => "투자 모듈 응답이 허용 크기를 넘었습니다.".to_owned(),
            Self::Status(status) => format!("투자 모듈이 HTTP {status}로 응답했습니다."),
        }
    }
}

fn investment_unavailable(error: ThesisReadError) -> InvestmentProjection {
    let (status, next_action) = match error {
        ThesisReadError::Unauthorized => (
            "needs_token",
            Some("THESIS_API_TOKEN을 설정한 뒤 mko ui를 다시 시작하세요.".to_owned()),
        ),
        _ => (
            "unavailable",
            Some("기존 Thesis 로컬 API를 시작한 뒤 새로고침하세요.".to_owned()),
        ),
    };
    InvestmentProjection {
        status,
        message: error.user_message(),
        next_action,
        source: "transitional_local_adapter",
        ..InvestmentProjection::default()
    }
}

fn investment_execution(item: ExecutionPlanPayload, state: &'static str) -> InvestmentExecution {
    InvestmentExecution {
        id: item.id,
        portfolio_decision_id: item.portfolio_decision_id,
        proposal_count: item.proposals.len(),
        notional: item
            .proposals
            .iter()
            .map(|proposal| proposal.notional)
            .sum(),
        state,
        created_at: item.created_at,
    }
}

#[derive(Deserialize)]
struct ApprovalInboxPayload {
    pending_filing_candidates: Vec<FilingPayload>,
    pending_evidence: Vec<EvidencePayload>,
    pending_portfolio_decisions: Vec<PortfolioDecisionPayload>,
    pending_execution_plans: Vec<ExecutionPlanPayload>,
    ready_paper_execution_plans: Vec<ExecutionPlanPayload>,
}

#[derive(Deserialize)]
struct FilingPayload {
    id: String,
    company_id: String,
    form: String,
    filing_date: String,
    filing_uri: String,
}

#[derive(Deserialize)]
struct EvidencePayload {
    id: String,
    company_id: String,
    source: SourcePayload,
    claim_key: String,
    relation: String,
    summary: String,
    locator: String,
}

#[derive(Deserialize)]
struct SourcePayload {
    title: String,
    uri: String,
}

#[derive(Deserialize)]
struct PortfolioDecisionPayload {
    id: String,
    portfolio_id: String,
    target: PortfolioTargetPayload,
    rationale: String,
    created_at: String,
}

#[derive(Deserialize)]
struct PortfolioTargetPayload {
    positions: Vec<serde_json::Value>,
    cash_weight: f64,
}

#[derive(Deserialize)]
struct ExecutionPlanPayload {
    id: String,
    portfolio_decision_id: String,
    proposals: Vec<OrderProposalPayload>,
    created_at: String,
}

#[derive(Deserialize)]
struct OrderProposalPayload {
    notional: f64,
}

#[derive(Deserialize)]
struct ThesisVersionPayload {
    thesis_id: String,
    company_id: String,
    version: u32,
    core_claim: String,
    claims: Vec<ThesisClaimPayload>,
    falsification_conditions: Vec<FalsificationPayload>,
}

#[derive(Clone, Deserialize, Serialize)]
struct ThesisClaimPayload {
    key: String,
    statement: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct FalsificationPayload {
    key: String,
    statement: String,
}

#[derive(Deserialize)]
struct ThesisPublicationPayload {
    version: u32,
    status: String,
    evidence_ids: Vec<String>,
    published_at: String,
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{Read, Write},
        net::{Shutdown, TcpListener, TcpStream},
        thread,
    };

    use mko_core::scaffold_v2::scaffold_personal_kb_v2;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn bind_and_thesis_endpoints_must_be_loopback() {
        assert!(parse_loopback_bind("127.0.0.1:2036").is_ok());
        assert_eq!(
            parse_loopback_bind("0.0.0.0:2036").unwrap_err().code(),
            "ui_bind_not_loopback"
        );
        let localhost = LocalHttpEndpoint::parse("http://localhost:2035").unwrap();
        assert_eq!(localhost.connect_host, "127.0.0.1");
        assert_eq!(
            LocalHttpEndpoint::parse("http://192.0.2.10:2035")
                .unwrap_err()
                .code(),
            "ui_thesis_url_not_loopback"
        );
        assert!(LocalHttpEndpoint::parse("https://127.0.0.1:2035").is_err());
        assert!(LocalHttpEndpoint::parse("http://127.0.0.1:2035/base%20path").is_ok());
        assert!(LocalHttpEndpoint::parse("http://127.0.0.1:2035/base path").is_err());
        assert!(ThesisReadClient::new(localhost, Some("bad\nheader".to_owned())).is_err());
    }

    #[test]
    fn request_parser_requires_complete_bounded_headers_and_one_host() {
        let options =
            read_request_over_tcp(b"OPTIONS * HTTP/1.1\r\nHost: 127.0.0.1:2036\r\n\r\n").unwrap();
        assert_eq!(options.method, "OPTIONS");

        let duplicate = read_request_over_tcp(
            b"GET / HTTP/1.1\r\nHost: 127.0.0.1:2036\r\nHost: localhost:2036\r\n\r\n",
        );
        assert!(duplicate.is_err());

        let incomplete = read_request_over_tcp(b"GET / HTTP/1.1\r\nHost: 127.0.0.1:2036\r\n");
        assert!(incomplete.is_err());

        let mut oversized = b"GET / HTTP/1.1\r\nHost: 127.0.0.1:2036\r\nX-Fill: ".to_vec();
        oversized.extend(std::iter::repeat_n(b'a', MAX_REQUEST_BYTES));
        oversized.extend_from_slice(b"\r\n\r\n");
        assert!(read_request_over_tcp(&oversized).is_err());
    }

    #[test]
    fn inbound_request_slow_drip_is_bounded_by_total_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            for byte in b"GET / HTTP/1.1\r\nHost: 127.0.0.1:2036\r\n\r\n" {
                thread::sleep(Duration::from_millis(250));
                if stream.write_all(&[*byte]).is_err() {
                    break;
                }
            }
        });
        let (mut stream, _) = listener.accept().unwrap();
        let started = Instant::now();

        let result = read_request(&mut stream);
        let elapsed = started.elapsed();
        drop(stream);
        client.join().unwrap();

        assert!(result.is_err());
        assert!(elapsed < Duration::from_secs(3));
    }

    #[test]
    fn empty_core_projection_is_read_only_and_uses_real_core_state() {
        let root = tempdir().unwrap();
        let repository = root.path().join("kb");
        let provider = root.path().join("provider");
        scaffold_personal_kb_v2(&repository).unwrap();
        fs::create_dir(&provider).unwrap();
        let before = snapshot(&repository);

        let projection = build_core_projection(&repository, &provider).unwrap();

        assert_eq!(projection.generation, "v3");
        assert_eq!(projection.counts.review_pending, 0);
        assert_eq!(projection.next_action, "지식 찾기");
        assert!(projection.queue.is_empty());
        assert_eq!(snapshot(&repository), before);
    }

    #[test]
    fn read_only_routes_reject_mutation_methods() {
        let root = tempdir().unwrap();
        let repository = root.path().join("kb");
        let provider = root.path().join("provider");
        scaffold_personal_kb_v2(&repository).unwrap();
        fs::create_dir(&provider).unwrap();
        let state = UiState {
            repository_root: repository,
            provider_root: provider,
            thesis: ThesisReadClient::new(
                LocalHttpEndpoint::parse("http://127.0.0.1:9").unwrap(),
                None,
            )
            .unwrap(),
            allowed_hosts: vec!["127.0.0.1:2036".to_owned()],
        };
        let request = IncomingRequest {
            method: "POST".to_owned(),
            target: "/api/projection".to_owned(),
            host: "127.0.0.1:2036".to_owned(),
        };

        let response = route_request(&request, &state);

        assert_eq!(response.status, 405);
        assert!(
            String::from_utf8(response.body)
                .unwrap()
                .contains("read_only")
        );
    }

    #[test]
    fn search_query_decodes_utf8_and_rejects_malformed_percent_encoding() {
        assert_eq!(
            parse_query("q=%EB%B3%91%EB%AA%A9&perspective=investment").unwrap(),
            vec![
                ("q".to_owned(), "병목".to_owned()),
                ("perspective".to_owned(), "investment".to_owned())
            ]
        );
        assert!(parse_query("q=%GG").is_err());

        let root = tempdir().unwrap();
        let repository = root.path().join("kb");
        scaffold_personal_kb_v2(&repository).unwrap();
        let error = match search_projection(&repository, "q=test&perspective=unknown") {
            Ok(_) => panic!("unknown perspective must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "ui_search_invalid");
    }

    #[test]
    fn thesis_adapter_reads_inbox_and_publication_without_exposing_token() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 2048];
                loop {
                    let read = stream.read(&mut chunk).unwrap();
                    bytes.extend_from_slice(&chunk[..read]);
                    if read == 0 || bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(bytes).unwrap();
                let path = request.split_whitespace().nth(1).unwrap().to_owned();
                requests.push(request);
                let body = match path.as_str() {
                    "/v1/approval-inbox" => {
                        r#"{"pending_filing_candidates":[],"pending_evidence":[],"pending_portfolio_decisions":[],"pending_execution_plans":[],"ready_paper_execution_plans":[]}"#
                    }
                    "/v1/theses" => {
                        r#"[{"thesis_id":"leu-2035","company_id":"LEU","version":2,"core_claim":"claim","claims":[],"falsification_conditions":[]}]"#
                    }
                    "/v1/theses/leu-2035/publication" => {
                        r#"{"version":1,"status":"active","evidence_ids":["ev-1"],"published_at":"2026-08-01T00:00:00Z"}"#
                    }
                    _ => panic!("unexpected path {path}"),
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
            requests
        });
        let client = ThesisReadClient::new(
            LocalHttpEndpoint::parse(&format!("http://{address}")).unwrap(),
            Some("test-secret".to_owned()),
        )
        .unwrap();

        let projection = client.read_projection();
        let requests = server.join().unwrap();

        assert_eq!(projection.status, "connected");
        assert_eq!(projection.theses.len(), 1);
        assert_eq!(projection.theses[0].version, 2);
        assert_eq!(projection.theses[0].publication_version, Some(1));
        assert_eq!(
            projection.theses[0].publication_status.as_deref(),
            Some("active")
        );
        assert!(
            requests
                .iter()
                .all(|request| request.contains("Authorization: Bearer test-secret"))
        );
        let serialized = serde_json::to_string(&projection).unwrap();
        assert!(!serialized.contains("test-secret"));
    }

    #[test]
    fn publication_lookup_failure_is_not_reported_as_unpublished() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let read = stream.read(&mut chunk).unwrap();
                    bytes.extend_from_slice(&chunk[..read]);
                    if read == 0 || bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let (status, body) = match index {
                    0 => (
                        "200 OK",
                        r#"{"pending_filing_candidates":[],"pending_evidence":[],"pending_portfolio_decisions":[],"pending_execution_plans":[],"ready_paper_execution_plans":[]}"#,
                    ),
                    1 => (
                        "200 OK",
                        r#"[{"thesis_id":"leu-2035","company_id":"LEU","version":2,"core_claim":"claim","claims":[],"falsification_conditions":[]}]"#,
                    ),
                    _ => ("500 Internal Server Error", r#"{"error":"failed"}"#),
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let client = ThesisReadClient::new(
            LocalHttpEndpoint::parse(&format!("http://{address}")).unwrap(),
            None,
        )
        .unwrap();

        let projection = client.read_projection();
        server.join().unwrap();

        assert_eq!(projection.status, "partial");
        assert_eq!(projection.theses[0].publication_state, "unknown");
        assert_eq!(projection.theses[0].publication_version, None);
    }

    #[test]
    fn thesis_slow_drip_is_bounded_by_total_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                .unwrap();
            for _ in 0..12 {
                thread::sleep(Duration::from_millis(250));
                if stream.write_all(b" ").is_err() {
                    break;
                }
            }
        });
        let client = ThesisReadClient::new(
            LocalHttpEndpoint::parse(&format!("http://{address}")).unwrap(),
            None,
        )
        .unwrap();
        let started = Instant::now();

        let projection = client.read_projection();
        server.join().unwrap();

        assert_eq!(projection.status, "unavailable");
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn chunked_response_decoder_is_bounded_and_correct() {
        assert_eq!(
            decode_chunked(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n").unwrap(),
            b"Wikipedia"
        );
        assert!(decode_chunked(b"4\r\nWiki").is_err());
        assert!(parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n[]").is_err());
        assert!(
            parse_http_response(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"
            )
            .is_err()
        );

        let oversized = HttpResponse::json(200, "x".repeat(MAX_UI_RESPONSE_BYTES + 1));
        assert_eq!(oversized.status, 500);
        assert!(oversized.body.len() < 256);
    }

    fn read_request_over_tcp(bytes: &[u8]) -> Result<IncomingRequest, MkoError> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let bytes = bytes.to_vec();
        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.write_all(&bytes).unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
        });
        let (mut stream, _) = listener.accept().unwrap();
        let result = read_request(&mut stream);
        client.join().unwrap();
        result
    }

    fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut entries = Vec::new();
        visit(root, root, &mut entries);
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    fn visit(root: &Path, path: &Path, entries: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut children = fs::read_dir(path)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let child_path = child.path();
            if child.file_type().unwrap().is_dir() {
                visit(root, &child_path, entries);
            } else {
                entries.push((
                    child_path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(child_path).unwrap(),
                ));
            }
        }
    }
}
