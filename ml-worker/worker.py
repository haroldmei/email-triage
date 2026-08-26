from __future__ import annotations

import argparse
import base64
import io
import json
import os
import re
from collections import defaultdict
from collections.abc import Iterable
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

# Paddle 3.x on Windows can route inference through a oneDNN/PIR executor path that fails for
# PP-OCR attributes. Disable those optional CPU paths before Paddle is imported.
os.environ.setdefault("FLAGS_use_mkldnn", "0")
os.environ.setdefault("FLAGS_use_onednn", "0")
os.environ.setdefault("FLAGS_enable_pir_api", "0")

import numpy as np
from PIL import Image

OCR_MODEL = None
NER_MODEL = None

GENERIC_NAME_TERMS = {
    "leeds china", "aus bachelor", "sg diploma", "pgt entry", "global leader",
    "meeting request", "officeworks invoice", "service agreement", "regional planning",
    "important reminder", "generally available", "ncuk mathematics", "your new",
    "opt placement", "opt placement support", "shinyway sydney", "shinyway australia",
    "english language", "english language requirements", "yes uk", "yes au", "pgt entry",
    "更新", "回复", "选择对应", "点击查看", "查看详情", "申请材料", "学生信息", "申请状态",
    "ocr error", "order no", "order number",
}

# These describe a student's own evidence/application packet. They are materially different from
# generic university brochures, course lists, entry requirements, or marketing information.
PERSONAL_MATERIAL_TERMS = [
    "passport", "transcript", "academic record", "offer letter", "letter of offer",
    "application form", "genuine student", "student declaration", "personal statement",
    "statement of purpose", "agent nomination", "authorisation", "authorization",
    "curriculum vitae", "resume", "student cv", "coe", "confirmation of enrolment",
    "visa", "ielts", "toefl", "pte", "certificate", "diploma", "degree certificate",
    "护照", "成绩单", "录取通知", "offer", "申请表", "学生申请信息表", "学生声明",
    "个人陈述", "授权书", "签证", "毕业证", "学位证", "在读证明", "语言成绩", "雅思", "托福",
]

GENERIC_UNIVERSITY_TERMS = [
    "entry requirements", "institution list", "course guide", "course handbook", "handbook",
    "brochure", "flyer", "prospectus", "price list", "newsletter", "marketing", "template",
    "course lookup", "quick query", "program guide", "programme guide", "admission guide",
    "application guide", "application guidelines", "international student course",
    "入学要求", "院校名单", "课程指南", "课程快速查询", "课程查询", "申请指南", "招生指南",
    "国际学生课程", "宣传", "海报", "模板",
]

NON_APPLICATION_TERMS = [
    "invoice", "receipt", "order no", "officeworks", "meeting request", "newsletter",
    "流水", "发票", "收据", "会议",
]

IDENTITY_LABEL_TERMS = [
    "student name", "applicant name", "full name", "chinese name", "english name",
    "学生姓名", "申请人姓名", "申请学生姓名", "中文姓名", "英文姓名", "姓名",
]

GENERIC_IMAGE_RE = re.compile(
    r"(?i)^(?:image\d*|insertpic[^.]*|catch[^.]*|[0-9a-f]{6,}[@._-].*)\.(?:png|jpe?g|gif|webp|bmp)$"
)
NER_LABELS = ["student chinese name", "student english name", "student name", "applicant name"]


@dataclass(frozen=True)
class NameEvidence:
    value: str
    source: str
    attachment: str
    score: float


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
            enable_mkldnn=False,
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


def run_ocr(image_bytes: bytes) -> tuple[str, tuple[int, int]]:
    image = Image.open(io.BytesIO(image_bytes)).convert("RGB")
    size = image.size
    array = np.ascontiguousarray(np.asarray(image))
    errors: list[str] = []
    for call in (
        lambda: OCR_MODEL.predict(array),
        lambda: OCR_MODEL.predict(input=array),
        lambda: OCR_MODEL.ocr(array),
    ):
        texts: list[str] = []
        try:
            flatten_ocr_result(call(), texts)
            unique = list(dict.fromkeys(texts))
            if unique:
                return "\n".join(unique), size
            errors.append("no recognized text")
        except Exception as exc:
            errors.append(f"{type(exc).__name__}: {exc}")
    raise RuntimeError("; ".join(errors)[:1200])


def clean_name(value: str) -> str:
    return re.sub(r"\s+", " ", value.strip(" \t\r\n:：,，;；._-"))


def plausible_student_name(value: str) -> bool:
    value = clean_name(value)
    lower = value.lower()
    if not value or lower in GENERIC_NAME_TERMS:
        return False
    if any(term in lower for term in ["university", "college", "bachelor", "master", "diploma", "entry", "requirement", "course", "program", "programme", "china", "australia"]):
        return False
    if any(term in value for term in ["大学", "学院", "本科", "硕士", "课程", "要求", "指南", "更新", "回复"]):
        return False
    if re.fullmatch(r"[\u4e00-\u9fff]{2,4}", value):
        return True
    if re.fullmatch(r"[A-Za-z][A-Za-z' -]{2,60}", value) and len(value.split()) in (2, 3):
        # All-uppercase surname + title-cased given name is especially common in application files.
        return True
    return False


def has_identity_signal(text: str) -> bool:
    lower = text.lower()
    if any(term in lower for term in IDENTITY_LABEL_TERMS):
        return True
    if re.search(r"(?i)\b(?:passport|application|student|candidate)\s*(?:no|number|id)\b", text):
        return True
    if re.search(r"(?:护照号|申请号|学生号|学号|出生日期)", text):
        return True
    return False


def classify_attachment(filename: str, content_type: str, text: str, image_size: tuple[int, int] | None) -> tuple[bool, str, float, str]:
    haystack = f"{filename}\n{text[:10000]}".lower()
    personal_hits = sum(1 for term in PERSONAL_MATERIAL_TERMS if term in haystack)
    generic_hits = sum(1 for term in GENERIC_UNIVERSITY_TERMS if term in haystack)
    unrelated_hits = sum(1 for term in NON_APPLICATION_TERMS if term in haystack)
    identity_signal = has_identity_signal(text)

    if image_size is not None:
        width, height = image_size
        if width * height < 50000 and len(text.strip()) < 25:
            return False, "decorative", 0.99, "small image with almost no OCR text"
        if GENERIC_IMAGE_RE.match(filename) and len(text.strip()) < 35 and personal_hits == 0:
            return False, "decorative", 0.97, "generic embedded image without personal-application evidence"

    # Generic university/course material is never student-specific merely because it contains
    # words like 'student', 'application', or 'university'.
    if generic_hits > 0 and personal_hits == 0 and not identity_signal:
        return False, "generic_university_material", min(0.99, 0.84 + 0.04 * generic_hits), "generic university/course material without student-specific identity evidence"
    if unrelated_hits > 0 and personal_hits == 0:
        return False, "non_application", min(0.99, 0.84 + 0.04 * unrelated_hits), "non-application business material"

    # Strong personal document types are retained even if the extracted text does not expose a
    # name; they can corroborate identity found in another attachment.
    if personal_hits >= 2:
        return True, "student_specific_application_material", min(0.99, 0.88 + 0.04 * personal_hits), "multiple personal application-document signals"
    if personal_hits == 1:
        return True, "student_specific_application_material", 0.84, "personal application-document signal"

    extension = filename.lower().rsplit(".", 1)[-1] if "." in filename else ""
    if extension in {"pdf", "docx", "doc", "xlsx", "xls", "csv"} and len(text.strip()) >= 200 and identity_signal:
        return True, "student_specific_application_material", 0.82, "document contains explicit student/applicant identity fields"

    return False, "non_application", 0.78, "no student-specific application evidence"


def explicit_name_evidence(filename: str, text: str) -> list[NameEvidence]:
    results: list[NameEvidence] = []
    patterns = [
        r"(?im)^\s*(?:学生姓名|申请人姓名|申请学生姓名|中文姓名|英文姓名|student\s*name|applicant\s*name|chinese\s*name|english\s*name|full\s*name)\s*[:：=]\s*([^\r\n]{2,60})\s*$",
        r"(?im)^\s*(?:学生姓名|申请人姓名|申请学生姓名|中文姓名|英文姓名|student\s*name|applicant\s*name|chinese\s*name|english\s*name|full\s*name)\s*$\r?\n\s*([^\r\n]{2,60})\s*$",
    ]
    for pattern in patterns:
        for match in re.finditer(pattern, text[:20000]):
            value = clean_name(match.group(1))
            if plausible_student_name(value):
                results.append(NameEvidence(value, "explicit_field", filename, 1.0))
    return results


def filename_name_evidence(filename: str) -> list[NameEvidence]:
    stem = filename.rsplit(".", 1)[0]
    results: list[NameEvidence] = []

    # Only extract names from an explicit filename segment/boundary. Do not search arbitrary two
    # title-cased words anywhere in a filename (the source of 'Leeds China').
    patterns = [
        r"[_–—-]([A-Z]{2,20}[ _-]+[A-Z][A-Za-z'-]{1,30})$",
        r"[_–—-]([A-Z][A-Za-z'-]{1,30}[ _-]+[A-Z][A-Za-z'-]{1,30})$",
        r"[_–—-]([\u4e00-\u9fff]{2,4})$",
        r"^([\u4e00-\u9fff]{2,4})(?:[_–— -]+)(?:护照|成绩单|申请表|申请材料|录取|offer)",
    ]
    for pattern in patterns:
        match = re.search(pattern, stem)
        if not match:
            continue
        value = clean_name(match.group(1).replace("_", " "))
        if plausible_student_name(value):
            results.append(NameEvidence(value, "filename_segment", filename, 0.88))
    return results


def ner_name_evidence(filename: str, text: str) -> list[NameEvidence]:
    results: list[NameEvidence] = []
    priorities = {"student chinese name": 4, "student name": 3, "applicant name": 3, "student english name": 2}
    for i in range(0, min(len(text), 16000), 4000):
        chunk = text[i:i + 4000]
        for entity in NER_MODEL.predict_entities(chunk, NER_LABELS, threshold=0.55):
            label = str(entity.get("label", "")).lower().strip()
            value = clean_name(str(entity.get("text", "")))
            score = float(entity.get("score", 0.0))
            if label in priorities and plausible_student_name(value):
                # Generic NER is supporting evidence only, never sufficient on its own.
                results.append(NameEvidence(value, f"ner:{label}", filename, min(0.70, score)))
    return results


def normalize_name(value: str) -> str:
    return re.sub(r"[^a-z0-9\u4e00-\u9fff]", "", value.lower())


def choose_student_name(relevant_docs: list[tuple[str, str]]) -> tuple[str | None, float, str]:
    evidence: list[NameEvidence] = []
    for filename, text in relevant_docs:
        evidence.extend(explicit_name_evidence(filename, text))
        evidence.extend(filename_name_evidence(filename))
        evidence.extend(ner_name_evidence(filename, text))

    groups: dict[str, list[NameEvidence]] = defaultdict(list)
    for item in evidence:
        groups[normalize_name(item.value)].append(item)

    accepted: list[tuple[float, str, list[NameEvidence]]] = []
    for items in groups.values():
        attachments = {item.attachment for item in items}
        explicit = [item for item in items if item.source == "explicit_field"]
        filename_hits = [item for item in items if item.source == "filename_segment"]
        ner_hits = [item for item in items if item.source.startswith("ner:")]

        # One explicit labelled field is strong enough. Otherwise require cross-attachment
        # corroboration from at least two filename segments, or a filename + independent NER hit.
        if explicit:
            confidence = 0.99
        elif len({item.attachment for item in filename_hits}) >= 2:
            confidence = 0.97
        elif filename_hits and ner_hits and len(attachments) >= 2:
            confidence = 0.93
        else:
            continue

        # Prefer Chinese when confidence/evidence are otherwise comparable, matching folder policy.
        representative = max(items, key=lambda item: (any('\u4e00' <= ch <= '\u9fff' for ch in item.value), item.score)).value
        accepted.append((confidence, representative, items))

    if not accepted:
        summary = "; ".join(f"{item.value}@{item.source}:{item.attachment}" for item in evidence[:12])
        return None, 0.0, f"no identity reached consensus; candidates={summary or 'none'}"

    accepted.sort(key=lambda row: (row[0], any('\u4e00' <= ch <= '\u9fff' for ch in row[1])), reverse=True)
    confidence, value, items = accepted[0]
    details = ", ".join(f"{item.source}:{item.attachment}" for item in items[:8])
    return value, confidence, f"identity consensus from {details}"


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
                if ocr_text.strip():
                    text = f"{text}\n{ocr_text}".strip()
            except Exception as exc:
                ocr_errors.append(f"{filename}: {type(exc).__name__}: {exc}"[:1400])

        relevant, category, score, reason = classify_attachment(filename, content_type, text, image_size)
        decisions.append({"filename": filename, "relevant": relevant, "category": category, "score": score, "reason": reason})
        if relevant:
            relevant_docs.append((filename, text))
            if data_b64 and text.strip():
                ocr_chunks.append(f"[ATTACHMENT {filename}]\n{text}")

    student_name, confidence, evidence = choose_student_name(relevant_docs) if relevant_docs else (None, 0.0, "no relevant student-specific application materials")
    return {
        "ocrText": "\n".join(ocr_chunks),
        "ocrErrors": ocr_errors,
        "attachmentDecisions": decisions,
        "studentName": student_name,
        "confidence": confidence,
        "evidence": evidence,
    }


class Handler(BaseHTTPRequestHandler):
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
