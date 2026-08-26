from __future__ import annotations

import argparse
import base64
import io
import json
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

import numpy as np
from PIL import Image

OCR_MODEL = None
NER_MODEL = None

BLOCKED_NAMES = {
    "选择对应", "点击查看", "查看详情", "申请材料", "相关文件", "学生信息", "申请状态",
    "上传文件", "查看文件", "附件信息", "申请信息", "文件材料", "对应材料",
}

NER_LABELS = [
    "student chinese name",
    "student english name",
    "student name",
    "applicant name",
    "application id",
    "university",
    "course",
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
        # PaddleOCR classic API commonly emits [box, (text, confidence)].
        if len(value) == 2 and isinstance(value[1], (list, tuple)) and value[1] and isinstance(value[1][0], str):
            flatten_ocr_result(value[1][0], output)
            return
        for item in value:
            flatten_ocr_result(item, output)
        return

    # PaddleOCR v3 result objects expose a json/res attribute depending on version.
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
    array = np.asarray(image)
    texts: list[str] = []

    # Prefer the newer predict API, then fall back to the classic ocr API.
    try:
        result = OCR_MODEL.predict(array)
    except Exception:
        result = OCR_MODEL.ocr(array)
    flatten_ocr_result(result, texts)

    # Deduplicate while preserving order; OCR engines can expose the same text in multiple fields.
    seen: set[str] = set()
    unique: list[str] = []
    for text in texts:
        if text not in seen:
            seen.add(text)
            unique.append(text)
    return "\n".join(unique)


def plausible_student_name(value: str) -> bool:
    value = value.strip(" \t\r\n:：,，;；.-")
    if not value or value in BLOCKED_NAMES:
        return False
    if re.fullmatch(r"[\u4e00-\u9fff]{2,4}", value):
        return True
    if re.fullmatch(r"[A-Za-z][A-Za-z' -]{2,60}", value) and len(value.split()) in (2, 3):
        return True
    return False


def extract_student_name(text: str) -> tuple[str | None, float, str]:
    # High precision structured fields first. These are preferable to generic NER.
    patterns = [
        r"(?im)^\s*(?:学生姓名|申请人姓名|中文姓名|中文名|student\s*name|applicant\s*name|chinese\s*name)\s*[:：]\s*([^\r\n]{2,60})\s*$",
        r"(?im)^\s*(?:学生姓名|申请人姓名|中文姓名|中文名|student\s*name|applicant\s*name|chinese\s*name)\s*$\r?\n\s*([^\r\n]{2,60})\s*$",
    ]
    for pattern in patterns:
        match = re.search(pattern, text)
        if match:
            value = match.group(1).strip()
            if plausible_student_name(value):
                return value, 0.99, "explicit labeled student/applicant name"

    entities = NER_MODEL.predict_entities(text[:12000], NER_LABELS, threshold=0.45)
    preferred_labels = {
        "student chinese name": 4,
        "student name": 3,
        "applicant name": 2,
        "student english name": 1,
    }
    ranked: list[tuple[int, float, str, str]] = []
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
    parts = [payload.get("subject") or "", payload.get("textBody") or "", payload.get("htmlBody") or ""]
    ocr_chunks: list[str] = []

    for attachment in payload.get("attachments") or []:
        try:
            data = base64.b64decode(attachment.get("dataBase64") or "", validate=False)
            if not data:
                continue
            text = run_ocr(data)
            if text.strip():
                filename = attachment.get("filename") or "image"
                ocr_chunks.append(f"[{filename}]\n{text}")
        except Exception as exc:
            ocr_chunks.append(f"[OCR_ERROR {attachment.get('filename', 'image')}: {exc}]")

    ocr_text = "\n".join(ocr_chunks)
    parts.append(ocr_text)
    combined = "\n".join(part for part in parts if part)
    student_name, confidence, evidence = extract_student_name(combined)

    return {
        "ocrText": ocr_text,
        "studentName": student_name,
        "confidence": confidence,
        "evidence": evidence,
    }


class Handler(BaseHTTPRequestHandler):
    server_version = "EmailTriageLocalML/0.1.19"

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
            result = handle_extract(payload)
            self._json(200, result)
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
