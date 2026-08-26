# Email Triage Local OCR/NER Worker

This test worker keeps student documents on the local machine and exposes only `127.0.0.1:8765`.

## Windows test

1. Install Python 3.10+ if it is not already installed.
2. Run `start-windows.ps1` in PowerShell.
3. On the first run, the script creates `.venv`, installs PaddleOCR + GLiNER CPU dependencies, and downloads the open model weights locally.
4. Leave the worker window running.
5. Start Email Triage 0.1.19 and run **Process now**.

The app falls back to its deterministic extractor if the worker is not running.

Models used:

- PaddleOCR: `PP-OCRv5_mobile_det` + `PP-OCRv5_server_rec`
- NER: `urchade/gliner_multi-v2.1`

The worker first prefers explicit fields such as `学生姓名`, `申请人姓名`, `Student Name`, and `Applicant Name`, then uses GLiNER when the text is less structured. Known UI phrases such as `选择对应` are rejected as student names.
