from __future__ import annotations

import argparse
import base64
import io
import json
import re
from collections.abc import Iterable
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

import numpy as np
from PIL import Image

OCR_MODEL = None
NER_MODEL = None

BLOCKED_NAMES = {
    "选择对应", "点击查看", "查看详情", "申请材料", "相关文件", "学生信息", "申请状态",
    "上传文件", "查看文件", "附件信息", "申请信息", "文件材料", "对应材料",
    "ocr error", "order no", "order number", "shinyway sydney",
}

NER_LABELS = [
    "student chinese name",
    "student english name",
    "student name",
    "applicant name",
]


def ensure_models() -> None:
    global OCR_MODEL, NER_MODEL
    if OCR_MODEL is None:
        from paddleocr import PaddleOCR

        OCR_MODEL = PaddleOCR(
            text_detection_model_name="PP-OCRv5_mobile_det",
            text_recognition_model_name="PP-OCRv5_server_rec",
            use_doc_orientation_classify=False,
            use_doc_unwarping=False,
            use_textline_orientation=False,
        )
    if NER_MODEL is None:
        from gliner import GLiNER

        NER_MODEL = GLiNER.from_pretrained("urchade/gliner_multi-v2.1")


def flatten_ocr_result(value: Any, output: list[str]) -> None:
    if value is None:
        return
    if isinstance(value, str):
        if value.strip():
            output.append(value.strip())
        return
    if isinstance(value, dict):
        for key in ("rec_texts", "text", "texts", "rec_text"):
            if key in value:
                flatten_ocr_result(value[key], output)
        return
    if isinstance(value, (list, tuple)):
        if len(value) == 2 and isinstance(value[1], (list, tuple)) and value[1] and isinstance(value[1][0], str):
            flatten_ocr_result(value[1][0], output)
            return
        for item in value:
            flatten_ocr_result(item, output)
        return
    # PaddleOCR 3 may return an iterator/generator of result objects.
    if isinstance(value, Iterable) and not isinstance(value, (bytes, bytearray)):
        try:
            for item in value:
                flatten_ocr_result(item, output)
            return
        except TypeError:
            pass

    for attr in ("json", "res"):
        if hasattr(value, attr):
            try:
                candidate = getattr(value, attr)
                if callable(candidate):
                    candidate = candidate()
                flatten_ocr_result(candidate, output)
                return
            except Exception:
                pass


def run_ocr(image_bytes: bytes) -> str:
    image = Image.open(io.BytesIO(image_bytes)).convert("RGB")
    array = np.ascontiguousarray(np.asarray(image))
    errors: list[str] = []
    for call in (
        lambda: OCR_MODEL.predict(array),
        lambda: OCR_MODEL.predict(input=array),
        lambda: OCR_MODEL.ocr(array),
    ):
        texts: list[str] = []
        try:
            result = call()
            flatten_ocr_result(result, texts)
            unique: list[str] = []
            seen: set[str] = set()
            for text in texts:
                if text not in seen:
                    seen.add(text)
                    unique.append(text)
            if unique:
                return "\n".join(unique)
            errors.append("OCR call returned no recognized text")
        except Exception as exc:
            errors.append(f"{type(exc).__name__}: {exc}")
    raise RuntimeError("; ".join(errors)[:1200])


def plausible_student_name(value: str) -> bool:
    value = value.strip(" \t\r\n:：,，;；.-")
    if not value or value.lower() in BLOCKED_NAMES:
        return False
    if re.fullmatch(r"[\u4e00-\u9fff]{2,4}", value):
        return True
    if re.fullmatch(r"[A-Za-z][A-Za-z' -]{2,60}", value) and len(value.split()) in (2, 3):
        return True
    return False


def filename_candidate(text: str) -> tuple[str | None, float, str]:
    for line in text.splitlines():
        if not line.startswith("[ATTACHMENT "):
            continue
        filename = line.removeprefix("[ATTACHMENT ").removesuffix("]").strip()
        stem = filename.rsplit(".", 1)[0]
        patterns = [
            r"(?:^|[_\-–— ])([A-Z]{2,20}[ _-]+[A-Z][A-Za-z'-]{1,30})(?:$|[_\-–— ])",
            r"(?:^|[_\-–— ])([A-Z][A-Za-z'-]{1,30}[ _-]+[A-Z][A-Za-z'-]{1,30})(?:$|[_\-–— ])",
            r"^([\u4e00-\u9fff]{2,4})(?:[_\-–— ]|的)(?:护照|成绩单|申请|申请表|材料|offer|录取)",
        ]
        for pattern in patterns:
            match = re.search(pattern, stem)
            if match:
                value = match.group(1).replace("_", " ").replace("-", " ").strip()
                if plausible_student_name(value):
                    return value, 0.97, f"student-like name in attachment filename {filename}"
    return None, 0.0, ""


def extract_student_name(text: str) -> tuple[str | None, float, str]:
    patterns = [
        r"(?im)^\s*(?:学生姓名|申请人姓名|申请学生姓名|中文姓名|中文名|student\s*name|applicant\s*name|chinese\s*name)\s*[:：=]\s*([^\r\n]{2,60})\s*$",
        r"(?im)^\s*(?:学生姓名|申请人姓名|申请学生姓名|中文姓名|中文名|student\s*name|applicant\s*name|chinese\s*name)\s*$\r?\n\s*([^\r\n]{2,60})\s*$",
    ]
    for pattern in patterns:
        match = re.search(pattern, text)
        if match:
            value = match.group(1).strip()
            if plausible_student_name(value):
                return value, 0.99, "explicit labeled student/applicant name"

    value, score, evidence = filename_candidate(text)
    if value:
        return value, score, evidence

    preferred_labels = {
        "student chinese name": 4,
        "student name": 3,
        "applicant name": 2,
        "student english name": 1,
    }
    ranked: list[tuple[int, float, str, str]] = []
    # Run a few bounded chunks instead of truncating the whole message at one arbitrary boundary.
    chunks = [text[i:i + 4000] for i in range(0, min(len(text), 20000), 4000)]
    for chunk in chunks:
        entities = NER_MODEL.predict_entities(chunk, NER_LABELS, threshold=0.40)
        for entity in entities:
            label = str(entity.get("label", "")).lower().strip()
            value = str(entity.get("text", "")).strip()
            score = float(entity.get("score", 0.0))
            if label in preferred_labels and plausible_student_name(value):
                ranked.append((preferred_labels[label], score, value, label))
    if not ranked:
        return None, 0.0, "no student/applicant entity"
    ranked.sort(key=lambda item: (item[0], item[1]), reverse=True)
    _, score, value, label = ranked[0]
    return value, score, f"GLiNER entity label={label}"


def handle_extract(payload: dict[str, Any]) -> dict[str, Any]:
    ensure_models()
    subject = payload.get("subject") or ""
    body = "\n".join([payload.get("textBody") or "", payload.get("htmlBody") or ""])
    document_text = payload.get("documentText") or ""
    ocr_chunks: list[str] = []
    ocr_errors: list[str] = []

    for attachment in payload.get("attachments") or []:
        try:
            data = base64.b64decode(attachment.get("dataBase64") or "", validate=False)
            if not data:
                continue
            text = run_ocr(data)
            if text.strip():
                filename = attachment.get("filename") or "image"
                ocr_chunks.append(f"[ATTACHMENT {filename}]\n{text}")
        except Exception as exc:
            filename = attachment.get("filename") or "image"
            ocr_errors.append(f"{filename}: {type(exc).__name__}: {exc}"[:1400])

    ocr_text = "\n".join(ocr_chunks)
    # Put high-signal attachment/filename content before generic email body for NER.
    combined = "\n".join(part for part in [subject, document_text[:12000], ocr_text[:12000], body[:6000]] if part)
    student_name, confidence, evidence = extract_student_name(combined)

    return {
        "ocrText": ocr_text,
        "ocrErrors": ocr_errors,
        "studentName": student_name,
        "confidence": confidence,
        "evidence": evidence,
    }


class Handler(BaseHTTPRequestHandler):
    server_version = "EmailTriageLocalML/0.1.20"

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


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--preload", action="store_true")
    args = parser.parse_args()
    if args.preload:
        ensure_models()
    print(f"Local OCR/NER worker listening on http://{args.host}:{args.port}", flush=True)
    ThreadingHTTPServer((args.host, args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
