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
    "ocr error", "order no", "order number", "shinyway sydney", "meeting request",
    "officeworks invoice", "global leader", "pgt entry",
}

APPLICATION_POSITIVE = [
    "passport", "transcript", "offer", "application", "applicant", "student declaration",
    "genuine student", "personal statement", "statement of purpose", "sop", "cv", "resume",
    "certificate", "diploma", "degree", "enrolment", "enrollment", "coe", "visa",
    "ielts", "toefl", "pte", "academic record", "agent nomination", "authorisation",
    "护照", "成绩单", "录取", "申请表", "申请信息", "大学申请", "学生声明", "授权书",
    "签证", "学历", "学位", "毕业证", "在读证明", "语言成绩", "雅思", "托福",
]
APPLICATION_NEGATIVE = [
    "invoice", "receipt", "order no", "officeworks", "meeting request", "newsletter",
    "entry requirements", "institution list", "course guide", "handbook", "brochure", "flyer",
    "price list", "marketing", "logo", "signature", "banner", "template", "quick query",
    "流水", "发票", "收据", "会议", "课程指南", "院校名单", "入学要求", "宣传", "海报",
]
GENERIC_IMAGE_RE = re.compile(r"(?i)^(?:image\d*|insertpic[^.]*|catch[^.]*|[0-9a-f]{6,}[@._-].*)\.(?:png|jpe?g|gif|webp|bmp)$")

NER_LABELS = ["student chinese name", "student english name", "student name", "applicant name"]


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
        if value.strip(): output.append(value.strip())
        return
    if isinstance(value, dict):
        for key in ("rec_texts", "text", "texts", "rec_text"):
            if key in value: flatten_ocr_result(value[key], output)
        return
    if isinstance(value, (list, tuple)):
        if len(value) == 2 and isinstance(value[1], (list, tuple)) and value[1] and isinstance(value[1][0], str):
            flatten_ocr_result(value[1][0], output); return
        for item in value: flatten_ocr_result(item, output)
        return
    if isinstance(value, Iterable) and not isinstance(value, (bytes, bytearray)):
        try:
            for item in value: flatten_ocr_result(item, output)
            return
        except TypeError:
            pass
    for attr in ("json", "res"):
        if hasattr(value, attr):
            try:
                candidate = getattr(value, attr)
                if callable(candidate): candidate = candidate()
                flatten_ocr_result(candidate, output); return
            except Exception:
                pass


def run_ocr(image_bytes: bytes) -> tuple[str, tuple[int, int]]:
    image = Image.open(io.BytesIO(image_bytes)).convert("RGB")
    size = image.size
    array = np.ascontiguousarray(np.asarray(image))
    errors: list[str] = []
    for call in (lambda: OCR_MODEL.predict(array), lambda: OCR_MODEL.predict(input=array), lambda: OCR_MODEL.ocr(array)):
        texts: list[str] = []
        try:
            flatten_ocr_result(call(), texts)
            unique = list(dict.fromkeys(texts))
            if unique: return "\n".join(unique), size
            errors.append("no recognized text")
        except Exception as exc:
            errors.append(f"{type(exc).__name__}: {exc}")
    raise RuntimeError("; ".join(errors)[:1200])


def plausible_student_name(value: str) -> bool:
    value = value.strip(" \t\r\n:：,，;；.-")
    if not value or value.lower() in BLOCKED_NAMES: return False
    if re.fullmatch(r"[\u4e00-\u9fff]{2,4}", value): return True
    return bool(re.fullmatch(r"[A-Za-z][A-Za-z' -]{2,60}", value) and len(value.split()) in (2, 3))


def classify_attachment(filename: str, content_type: str, text: str, image_size: tuple[int, int] | None) -> tuple[bool, str, float, str]:
    haystack = f"{filename}\n{text[:8000]}".lower()
    positive = sum(1 for term in APPLICATION_POSITIVE if term in haystack)
    negative = sum(1 for term in APPLICATION_NEGATIVE if term in haystack)

    if image_size is not None:
        width, height = image_size
        if width * height < 50000 and len(text.strip()) < 25:
            return False, "decorative", 0.99, "small image with almost no OCR text"
        if GENERIC_IMAGE_RE.match(filename) and len(text.strip()) < 35 and positive == 0:
            return False, "decorative", 0.95, "generic embedded image with no application signal"

    if negative > 0 and positive == 0:
        return False, "non_application", min(0.99, 0.75 + 0.08 * negative), "non-application keywords"
    if positive >= 2:
        return True, "application_material", min(0.99, 0.78 + 0.06 * positive), "multiple application-material signals"
    if positive == 1:
        return True, "application_material", 0.76, "application-material keyword"

    extension = filename.lower().rsplit(".", 1)[-1] if "." in filename else ""
    if extension in {"pdf", "docx", "doc", "xlsx", "xls", "csv"} and len(text.strip()) >= 250:
        contextual = any(term in haystack for term in ["university", "student", "applicant", "admission", "course", "学校", "大学", "学生", "申请"])
        if contextual:
            return True, "application_material", 0.68, "document contains student/admission context"

    return False, "non_application", 0.70, "no student-application evidence"


def filename_candidate(filename: str) -> tuple[str | None, float, str]:
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
            if plausible_student_name(value): return value, 0.97, f"name in relevant attachment filename {filename}"
    return None, 0.0, ""


def extract_student_name(relevant_docs: list[tuple[str, str]]) -> tuple[str | None, float, str]:
    combined = "\n".join(f"[ATTACHMENT {name}]\n{text}" for name, text in relevant_docs)
    for pattern in [
        r"(?im)^\s*(?:学生姓名|申请人姓名|申请学生姓名|中文姓名|中文名|student\s*name|applicant\s*name|chinese\s*name)\s*[:：=]\s*([^\r\n]{2,60})\s*$",
        r"(?im)^\s*(?:学生姓名|申请人姓名|申请学生姓名|中文姓名|中文名|student\s*name|applicant\s*name|chinese\s*name)\s*$\r?\n\s*([^\r\n]{2,60})\s*$",
    ]:
        match = re.search(pattern, combined)
        if match and plausible_student_name(match.group(1)):
            return match.group(1).strip(), 0.99, "explicit labeled name in application material"

    for filename, _ in relevant_docs:
        value, score, evidence = filename_candidate(filename)
        if value: return value, score, evidence

    ranked: list[tuple[int, float, str, str]] = []
    priorities = {"student chinese name": 4, "student name": 3, "applicant name": 2, "student english name": 1}
    for i in range(0, min(len(combined), 24000), 4000):
        for entity in NER_MODEL.predict_entities(combined[i:i+4000], NER_LABELS, threshold=0.45):
            label = str(entity.get("label", "")).lower().strip()
            value = str(entity.get("text", "")).strip()
            score = float(entity.get("score", 0.0))
            if label in priorities and plausible_student_name(value): ranked.append((priorities[label], score, value, label))
    if not ranked: return None, 0.0, "no student/applicant identity in relevant materials"
    ranked.sort(key=lambda item: (item[0], item[1]), reverse=True)
    _, score, value, label = ranked[0]
    return value, score, f"GLiNER on relevant application material label={label}"


def handle_extract(payload: dict[str, Any]) -> dict[str, Any]:
    ensure_models()
    decisions: list[dict[str, Any]] = []
    relevant_docs: list[tuple[str, str]] = []
    ocr_chunks: list[str] = []
    ocr_errors: list[str] = []

    for attachment in payload.get("attachments") or []:
        filename = attachment.get("filename") or "attachment"
        content_type = attachment.get("contentType") or ""
        text = attachment.get("extractedText") or ""
        image_size = None
        data_b64 = attachment.get("dataBase64") or ""
        if data_b64:
            try:
                ocr_text, image_size = run_ocr(base64.b64decode(data_b64, validate=False))
                if ocr_text.strip(): text = f"{text}\n{ocr_text}".strip()
            except Exception as exc:
                ocr_errors.append(f"{filename}: {type(exc).__name__}: {exc}"[:1400])

        relevant, category, score, reason = classify_attachment(filename, content_type, text, image_size)
        decisions.append({"filename": filename, "relevant": relevant, "category": category, "score": score, "reason": reason})
        if relevant:
            relevant_docs.append((filename, text))
            if data_b64 and text.strip(): ocr_chunks.append(f"[ATTACHMENT {filename}]\n{text}")

    student_name, confidence, evidence = extract_student_name(relevant_docs) if relevant_docs else (None, 0.0, "no relevant application materials")
    return {
        "ocrText": "\n".join(ocr_chunks),
        "ocrErrors": ocr_errors,
        "attachmentDecisions": decisions,
        "studentName": student_name,
        "confidence": confidence,
        "evidence": evidence,
    }


class Handler(BaseHTTPRequestHandler):
    server_version = "EmailTriageLocalML/0.1.21"
    def log_message(self, fmt: str, *args: Any) -> None: print(fmt % args, flush=True)
    def _json(self, status: int, payload: dict[str, Any]) -> None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(status); self.send_header("Content-Type", "application/json; charset=utf-8"); self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self) -> None:
        self._json(200, {"ok": True, "modelsLoaded": OCR_MODEL is not None and NER_MODEL is not None}) if self.path == "/health" else self._json(404, {"error": "not found"})
    def do_POST(self) -> None:
        if self.path != "/extract": self._json(404, {"error": "not found"}); return
        try:
            length = int(self.headers.get("Content-Length", "0")); payload = json.loads(self.rfile.read(length).decode("utf-8")); self._json(200, handle_extract(payload))
        except Exception as exc: self._json(500, {"error": str(exc)})


def main() -> None:
    parser = argparse.ArgumentParser(); parser.add_argument("--host", default="127.0.0.1"); parser.add_argument("--port", type=int, default=8765); parser.add_argument("--preload", action="store_true"); args = parser.parse_args()
    if args.preload: ensure_models()
    print(f"Local OCR/NER worker listening on http://{args.host}:{args.port}", flush=True)
    ThreadingHTTPServer((args.host, args.port), Handler).serve_forever()


if __name__ == "__main__": main()
