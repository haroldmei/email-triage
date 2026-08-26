from pathlib import Path


def replace_once(path: str, old: str, new: str, already: str | None = None) -> None:
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    if (already is not None and already in text) or new in text:
        return
    if old not in text:
        raise SystemExit(f"Expected v0.1.23 patch anchor not found in {path}: {old[:180]!r}")
    p.write_text(text.replace(old, new, 1), encoding="utf-8")


# The old 90-second total timeout can expire while CPU OCR/NER is still running. Give local
# inference a realistic upper bound while keeping connection establishment short.
replace_once(
    "src-tauri/src/local_ml.rs",
    'use std::time::Duration;',
    'use std::time::{Duration, Instant};',
    'use std::time::{Duration, Instant};',
)
replace_once(
    "src-tauri/src/local_ml.rs",
    'const LOCAL_ML_URL: &str = "http://127.0.0.1:8765/extract";',
    'const LOCAL_ML_URL: &str = "http://127.0.0.1:8765/extract";\nconst LOCAL_ML_CONNECT_TIMEOUT_SECS: u64 = 5;\nconst LOCAL_ML_REQUEST_TIMEOUT_SECS: u64 = 600;',
    'const LOCAL_ML_REQUEST_TIMEOUT_SECS: u64 = 600;',
)
replace_once(
    "src-tauri/src/local_ml.rs",
    '    let client = match reqwest::Client::builder().timeout(Duration::from_secs(90)).build() {',
    '''    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(LOCAL_ML_CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(LOCAL_ML_REQUEST_TIMEOUT_SECS))
        .build()
    {''',
    '.timeout(Duration::from_secs(LOCAL_ML_REQUEST_TIMEOUT_SECS))',
)

# Record end-to-end ML request latency in the desktop log. This is enough to tell whether a future
# failure is connection setup, inference timeout, HTTP 500, or response decoding.
replace_once(
    "src-tauri/src/local_ml.rs",
    '    let response = match client.post(LOCAL_ML_URL).json(&request).send().await {',
    '''    let request_started = Instant::now();
    logging::write(
        app,
        "INFO",
        format!(
            "stage=local_ml_request uid={uid} action=start attachments={} timeout_secs={LOCAL_ML_REQUEST_TIMEOUT_SECS}",
            message.attachments.len()
        ),
    );

    let response = match client.post(LOCAL_ML_URL).json(&request).send().await {''',
    'stage=local_ml_request uid={uid} action=start',
)
replace_once(
    "src-tauri/src/local_ml.rs",
    '''        Err(error) => {
            logging::write(app, "WARN", format!("stage=local_ml uid={uid} available=false reason=worker_unavailable error=\\"{}\\"", error.to_string().replace('"', "'")));
            return fallback(message);
        }
    };
''',
    '''        Err(error) => {
            logging::write(
                app,
                "WARN",
                format!(
                    "stage=local_ml_request uid={uid} action=failed elapsed_ms={} error=\\"{}\\"",
                    request_started.elapsed().as_millis(),
                    error.to_string().replace('"', "'")
                ),
            );
            return fallback(message);
        }
    };
''',
    'stage=local_ml_request uid={uid} action=failed elapsed_ms=',
)

# The model objects are process-global. Serialize inference requests so repeated clicks cannot run
# Paddle/GLiNER concurrently and multiply CPU latency.
replace_once(
    "ml-worker/worker.py",
    'import re\nfrom collections import defaultdict',
    'import re\nimport threading\nimport time\nimport uuid\nfrom collections import defaultdict',
    'import threading\nimport time\nimport uuid',
)
replace_once(
    "ml-worker/worker.py",
    'OCR_MODEL = None\nNER_MODEL = None',
    'OCR_MODEL = None\nNER_MODEL = None\nMODEL_INFERENCE_LOCK = threading.Lock()',
    'MODEL_INFERENCE_LOCK = threading.Lock()',
)

old_handler = '''class Handler(BaseHTTPRequestHandler):
    server_version = "EmailTriageLocalML/0.1.22"

    def log_message(self, fmt: str, *args: Any) -> None:
        print(fmt % args, flush=True)

    def _json(self, status: int, payload: dict[str, Any]) -> None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        if self.path == "/health":
            self._json(200, {"ok": True, "modelsLoaded": OCR_MODEL is not None and NER_MODEL is not None})
        else:
            self._json(404, {"error": "not found"})

    def do_POST(self) -> None:
        if self.path != "/extract":
            self._json(404, {"error": "not found"})
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
            payload = json.loads(self.rfile.read(length).decode("utf-8"))
            self._json(200, handle_extract(payload))
        except Exception as exc:
            self._json(500, {"error": str(exc)})
'''

new_handler = '''class Handler(BaseHTTPRequestHandler):
    server_version = "EmailTriageLocalML/0.1.23"

    def log_message(self, fmt: str, *args: Any) -> None:
        print(fmt % args, flush=True)

    def _json(self, status: int, payload: dict[str, Any], request_id: str = "-") -> bool:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        try:
            self.send_response(status)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return True
        except (BrokenPipeError, ConnectionAbortedError, ConnectionResetError, OSError) as exc:
            print(
                f"stage=http_response request_id={request_id} client_disconnected=true error={type(exc).__name__}: {exc}",
                flush=True,
            )
            return False

    def do_GET(self) -> None:
        if self.path == "/health":
            self._json(200, {"ok": True, "modelsLoaded": OCR_MODEL is not None and NER_MODEL is not None})
        else:
            self._json(404, {"error": "not found"})

    def do_POST(self) -> None:
        request_id = uuid.uuid4().hex[:10]
        request_started = time.perf_counter()
        if self.path != "/extract":
            self._json(404, {"error": "not found"}, request_id)
            return

        try:
            length = int(self.headers.get("Content-Length", "0"))
            payload = json.loads(self.rfile.read(length).decode("utf-8"))
        except Exception as exc:
            print(
                f"stage=http_request request_id={request_id} action=parse_failed error={type(exc).__name__}: {exc}",
                flush=True,
            )
            self._json(400, {"error": f"invalid request: {exc}"}, request_id)
            return

        attachment_count = len(payload.get("attachments") or [])
        print(
            f"stage=http_request request_id={request_id} action=start attachments={attachment_count} content_length={length}",
            flush=True,
        )

        try:
            lock_started = time.perf_counter()
            with MODEL_INFERENCE_LOCK:
                queue_ms = int((time.perf_counter() - lock_started) * 1000)
                result = handle_extract(payload)
            elapsed_ms = int((time.perf_counter() - request_started) * 1000)
            print(
                f"stage=http_request request_id={request_id} action=complete elapsed_ms={elapsed_ms} queue_ms={queue_ms}",
                flush=True,
            )
            self._json(200, result, request_id)
        except Exception as exc:
            elapsed_ms = int((time.perf_counter() - request_started) * 1000)
            print(
                f"stage=http_request request_id={request_id} action=processing_failed elapsed_ms={elapsed_ms} error={type(exc).__name__}: {exc}",
                flush=True,
            )
            self._json(500, {"error": str(exc)}, request_id)
'''
replace_once(
    "ml-worker/worker.py",
    old_handler,
    new_handler,
    'server_version = "EmailTriageLocalML/0.1.23"',
)
replace_once(
    "ml-worker/worker.py",
    '    ThreadingHTTPServer((args.host, args.port), Handler).serve_forever()',
    '''    server = ThreadingHTTPServer((args.host, args.port), Handler)
    server.daemon_threads = True
    server.allow_reuse_address = True
    server.serve_forever()''',
    'server.daemon_threads = True',
)

replace_once(
    "ml-worker/start-windows.ps1",
    ".email-triage-ml-ready-v0122",
    ".email-triage-ml-ready-v0123",
    ".email-triage-ml-ready-v0123",
)
replace_once(
    "ml-worker/start-windows.ps1",
    "for 0.1.22.",
    "for 0.1.23.",
    "for 0.1.23.",
)
replace_once(
    "ml-worker/start-windows.ps1",
    "worker 0.1.22 on 127.0.0.1:8765.",
    "worker 0.1.23 on 127.0.0.1:8765.",
    "worker 0.1.23 on 127.0.0.1:8765.",
)

print("Applied ML worker timeout/disconnect/concurrency resilience v0.1.23")
