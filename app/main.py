from __future__ import annotations

import hashlib
import io
import json
import math
import os
import re
import shutil
import sqlite3
import threading
import time
import uuid
import zipfile
from copy import deepcopy
from datetime import datetime
from itertools import pairwise
from pathlib import Path
from typing import Any

from fastapi import FastAPI, File, Form, HTTPException, Query, UploadFile
from fastapi.responses import FileResponse, JSONResponse
from fastapi.staticfiles import StaticFiles
from PIL import Image, ImageOps
from pydantic import BaseModel, Field, ValidationError

# The service is deliberately CPU-only.  Presidio's newer analyzer package
# lazily imports PyTorch to auto-detect a device; on some Windows installs the
# optional torch DLLs are unavailable even though the regex recognizers work
# perfectly well.  Pinning the device prevents that optional import from
# turning an otherwise usable analysis request into a 500 response.
os.environ.setdefault('PRESIDIO_DEVICE', 'cpu')

# Import the common result protocol before PaddleOCR can load its native
# runtime. On Windows, importing Presidio/PyTorch for the first time after
# Paddle has initialized may leave ``torch\lib\shm.dll`` unresolved. Caching
# the type here also avoids repeating a heavy optional import on each request.
try:
    from presidio_analyzer import RecognizerResult as PresidioRecognizerResult
except Exception:
    PresidioRecognizerResult = None

from .file_handlers import (
    SUPPORTED,
    content_manifest,
    extension,
    extract_text,
    mask_file,
)
from .ner import MODEL_ID, RaanerService
from .ocr import OCRService
from .policies import DEFAULT_POLICIES, POLICY_VERSION, apply_entities
from .security import decrypt_json, encrypt_json, validate_password

ROOT = Path(__file__).resolve().parent.parent
DATA = Path(os.getenv('LOCAL_DESENSITIZATION_DATA_DIR', str(ROOT / 'data'))).expanduser().resolve()
TASKS = DATA / 'tasks'
MODELS = Path(os.getenv('LOCAL_DESENSITIZATION_MODEL_DIR', str(ROOT / 'models'))).expanduser().resolve()
for p in (DATA,TASKS,MODELS): p.mkdir(parents=True, exist_ok=True)
DB=DATA/'tasks.db'; MAX_UPLOAD=50*1024*1024; MAX_BATCH_FILES=int(os.getenv('MAX_BATCH_FILES','20')); MAX_BATCH_BYTES=int(os.getenv('MAX_BATCH_BYTES',str(500*1024*1024))); TASK_TTL_HOURS=int(os.getenv('TASK_TTL_HOURS','24')); CLEANUP_INTERVAL_SECONDS=max(10, int(os.getenv('CLEANUP_INTERVAL_SECONDS', '300'))); MAX_ENTITIES=10000; MAX_IMAGE_BOXES=10000; MAX_IMAGE_PIXELS=int(os.getenv('MAX_IMAGE_PIXELS',str(40_000_000))); MAX_PDF_OCR_PAGES=int(os.getenv('MAX_PDF_OCR_PAGES','200')); MAX_PDF_OCR_PIXELS=int(os.getenv('MAX_PDF_OCR_PIXELS',str(12_000_000))); jobs={}; batches={}; policy_store=dict(DEFAULT_POLICIES); _POLICY_STORE_LOADED=False; ner_service=RaanerService(MODELS/'raner'); ocr_service=OCRService(MODELS/'paddleocr')
TASK_ID_RE = re.compile(r'^[0-9a-f]{32}$', re.IGNORECASE)
IMAGE_EXTENSIONS = {'png', 'jpg', 'jpeg', 'bmp', 'tif', 'tiff'}
app=FastAPI(title='本地数据脱敏系统',version='0.2.0')
_cleanup_lock = threading.Lock()
_last_cleanup = 0.0
_model_init_lock = threading.Lock()
_active_model_job: str | None = None
_model_state = 'starting'
_AUTO_INIT_ENABLED = os.getenv('LOCAL_DESENSITIZATION_AUTO_INIT', '1').lower() not in {'0', 'false', 'no'}
_TEST_MODE = os.getenv('LOCAL_DESENSITIZATION_TEST_MODE', '').lower() in {'1', 'true', 'yes'}

def models_ready() -> bool:
    return bool(ner_service.available and ocr_service.available)

def _model_gate_response():
    status = model_status()
    return JSONResponse(status_code=503, content={'detail':'模型尚未就绪，分析和脱敏暂不可用','code':'MODELS_NOT_READY','models':status})

@app.on_event('startup')
async def startup_models():
    if not _AUTO_INIT_ENABLED or _TEST_MODE:
        return
    # Start initialization and wait for the first attempt. Analysis remains
    # locked behind the middleware until both local models are ready.
    result = initialize()
    job_id = result.get('job_id') if isinstance(result, dict) else None
    timeout = max(1, int(os.getenv('MODEL_INIT_TIMEOUT_SECONDS', '900')))
    started = time.time()
    while job_id and time.time() - started < timeout:
        job = jobs.get(job_id, {})
        if job.get('status') in {'completed', 'error'}:
            break
        await __import__('asyncio').sleep(0.25)

def conn():
    c=sqlite3.connect(DB,check_same_thread=False, timeout=10)
    c.execute('PRAGMA journal_mode=WAL'); c.execute('PRAGMA busy_timeout=10000'); c.execute('PRAGMA foreign_keys=ON')
    c.execute('CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY, original TEXT, masked TEXT, created REAL, kind TEXT DEFAULT "text")')
    if 'kind' not in {row[1] for row in c.execute('PRAGMA table_info(tasks)').fetchall()}:
        c.execute('ALTER TABLE tasks ADD COLUMN kind TEXT DEFAULT "text"')
    columns = {row[1] for row in c.execute('PRAGMA table_info(tasks)').fetchall()}
    migrations = {
        'original_name': 'TEXT DEFAULT ""', 'source_path': 'TEXT DEFAULT ""',
        'artifact_name': 'TEXT DEFAULT ""', 'source_sha256': 'TEXT DEFAULT ""',
        'status': 'TEXT DEFAULT "created"', 'reversible': 'INTEGER DEFAULT 0',
        'batch_id': 'TEXT DEFAULT ""', 'metadata_json': 'TEXT DEFAULT "{}"',
        'restored_name': 'TEXT DEFAULT ""',
        'updated': 'REAL DEFAULT 0', 'error': 'TEXT DEFAULT ""',
    }
    for name, definition in migrations.items():
        if name not in columns:
            c.execute(f'ALTER TABLE tasks ADD COLUMN {name} {definition}')
    c.execute('CREATE TABLE IF NOT EXISTS rules(id TEXT PRIMARY KEY,name TEXT,kind TEXT,pattern TEXT,entity_type TEXT,replacement TEXT,enabled INTEGER DEFAULT 1)')
    c.execute('CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL)')
    c.execute('''CREATE TABLE IF NOT EXISTS batches(
        id TEXT PRIMARY KEY, created REAL NOT NULL, updated REAL NOT NULL,
        status TEXT NOT NULL, total INTEGER DEFAULT 0, completed INTEGER DEFAULT 0,
        failed INTEGER DEFAULT 0, archive_name TEXT DEFAULT '', metadata_json TEXT DEFAULT '{}',
        error TEXT DEFAULT '')''')
    c.commit(); return c


def _load_policy_store() -> None:
    """Load persisted policy overrides once, retaining built-in defaults."""
    global _POLICY_STORE_LOADED
    if _POLICY_STORE_LOADED:
        return
    c = conn()
    row = c.execute('SELECT value FROM settings WHERE key=?', ('policies',)).fetchone()
    c.close()
    if row:
        try:
            stored = json.loads(str(row[0]))
            if isinstance(stored, dict):
                policy_store.update(_validate_policy_map(stored))
        except (TypeError, ValueError, json.JSONDecodeError, HTTPException):
            # A malformed local setting must not prevent the rule engine from
            # starting; built-in policies remain the safe fallback.
            pass
    _POLICY_STORE_LOADED = True


def _persist_policy_store() -> None:
    c = conn()
    c.execute(
        'INSERT INTO settings(key,value) VALUES(?,?) '
        'ON CONFLICT(key) DO UPDATE SET value=excluded.value',
        ('policies', json.dumps(policy_store, ensure_ascii=False, separators=(',', ':'))),
    )
    c.commit()
    c.close()

def _valid_task_id(value: str) -> bool:
    return bool(isinstance(value, str) and TASK_ID_RE.fullmatch(value))

def _task_path(task_id: str, *, create: bool = False) -> Path:
    if not _valid_task_id(task_id):
        raise HTTPException(404, 'task not found')
    base = TASKS.resolve(); path = (base / task_id).resolve()
    if path.parent != base:
        raise HTTPException(404, 'task not found')
    if create:
        path.mkdir(parents=True, exist_ok=True)
    return path

def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()

def _safe_filename(filename: str) -> str:
    name = Path(filename or 'upload.bin').name.replace('\x00', '')
    if not name or name in {'.', '..'}:
        return 'upload.bin'
    return name[:240]

def _validate_upload(data: bytes, filename: str, ext: str) -> None:
    """Validate the file signature before handing bytes to a parser."""
    if ext in {'txt', 'md'}:
        if b'\x00' in data[:4096]:
            raise HTTPException(415, '文本文件包含二进制内容')
        return
    if ext == 'pdf':
        if not data.lstrip().startswith(b'%PDF-'):
            raise HTTPException(415, 'PDF 文件签名无效')
        document = None
        try:
            import fitz
            document = fitz.open(stream=data, filetype='pdf')
            if document.needs_pass:
                raise HTTPException(415, '不支持密码保护的 PDF 文件')
            if document.page_count > 1000:
                raise HTTPException(413, 'PDF 页数超过 1000 页')
        except HTTPException:
            raise
        except Exception as exc:
            raise HTTPException(415, f'无法读取 PDF: {exc}') from exc
        finally:
            if document is not None:
                document.close()
        return
    if ext in {'docx', 'xlsx', 'xlsm'}:
        if not data.startswith(b'PK\x03\x04'):
            raise HTTPException(415, 'Office 文件签名无效')
        try:
            with zipfile.ZipFile(io.BytesIO(data)) as archive:
                infos = archive.infolist()
                if len(infos) > 10000 or sum(max(0, item.file_size) for item in infos) > 500 * 1024 * 1024:
                    raise HTTPException(413, 'Office 压缩包内容过大')
                if any(item.flag_bits & 0x1 for item in infos):
                    raise HTTPException(415, '不支持加密的 Office 文件')
                names = set(archive.namelist())
                if ext == 'docx' and 'word/document.xml' not in names:
                    raise HTTPException(415, 'DOCX 结构无效')
                if ext in {'xlsx', 'xlsm'} and 'xl/workbook.xml' not in names:
                    raise HTTPException(415, 'Excel 结构无效')
        except HTTPException:
            raise
        except (OSError, zipfile.BadZipFile) as exc:
            raise HTTPException(415, f'Office 文件损坏: {exc}') from exc
        return
    if ext in IMAGE_EXTENSIONS:
        try:
            with Image.open(io.BytesIO(data)) as image:
                width, height = image.size
                if width <= 0 or height <= 0 or width * height > MAX_IMAGE_PIXELS:
                    raise HTTPException(413, f'图片像素超过限制（最多 {MAX_IMAGE_PIXELS}）')
                image.verify()
        except HTTPException:
            raise
        except Exception as exc:
            raise HTTPException(415, f'图片文件损坏: {exc}') from exc
        return
    raise HTTPException(415, f'不支持的文件格式: .{ext}')
def cleanup(*, force: bool = True) -> None:
    """Remove expired tasks and orphaned parser directories.

    Middleware calls are throttled because walking the task directory on every
    polling request makes large batches unnecessarily expensive. Direct calls
    (including maintenance scripts and tests) remain immediate by default.
    """
    global _last_cleanup
    now = time.time()
    if not force and now - _last_cleanup < CLEANUP_INTERVAL_SECONDS:
        return
    if not _cleanup_lock.acquire(blocking=False):
        return
    try:
        now = time.time()
        if not force and now - _last_cleanup < CLEANUP_INTERVAL_SECONDS:
            return
        cutoff = now - TASK_TTL_HOURS * 3600
        connection = conn()
        task_rows = connection.execute(
            'SELECT id FROM tasks WHERE COALESCE(NULLIF(updated,0),created)<?',
            (cutoff,),
        ).fetchall()
        batch_rows = connection.execute(
            'SELECT id FROM batches WHERE updated<?', (cutoff,)
        ).fetchall()
        expired_batches = {
            str(row[0]) for row in batch_rows if _valid_task_id(str(row[0]))
        }
        expired_tasks = {
            str(row[0]) for row in task_rows if _valid_task_id(str(row[0]))
        }
        if expired_batches:
            placeholders = ','.join('?' for _ in expired_batches)
            expired_tasks.update(
                str(row[0])
                for row in connection.execute(
                    f'SELECT id FROM tasks WHERE batch_id IN ({placeholders})',
                    tuple(expired_batches),
                ).fetchall()
                if _valid_task_id(str(row[0]))
            )
        for task_id in expired_tasks:
            shutil.rmtree(TASKS / task_id, ignore_errors=True)
        for batch_id in expired_batches:
            shutil.rmtree(TASKS / batch_id, ignore_errors=True)
        if expired_tasks:
            placeholders = ','.join('?' for _ in expired_tasks)
            connection.execute(
                f'DELETE FROM tasks WHERE id IN ({placeholders})',
                tuple(expired_tasks),
            )
        if expired_batches:
            placeholders = ','.join('?' for _ in expired_batches)
            connection.execute(
                f'DELETE FROM batches WHERE id IN ({placeholders})',
                tuple(expired_batches),
            )
        connection.commit()
        referenced = {
            str(row[0])
            for row in connection.execute(
                'SELECT id FROM tasks UNION SELECT id FROM batches'
            ).fetchall()
        }
        connection.close()
        # Recover from interrupted parsers which exited after creating a
        # directory but before committing the corresponding task row.
        for path in TASKS.iterdir():
            try:
                if (
                    path.is_dir()
                    and _valid_task_id(path.name)
                    and path.name not in referenced
                    and path.stat().st_mtime < cutoff
                ):
                    shutil.rmtree(path, ignore_errors=True)
            except OSError:
                continue
        _last_cleanup = now
    finally:
        _cleanup_lock.release()
def valid_id(v):
    if not re.fullmatch(r'\d{17}[\dXx]',v): return False
    try: datetime.strptime(v[6:14],'%Y%m%d')
    except ValueError: return False
    return '10X98765432'[sum(int(x)*w for x,w in zip(v[:17],[7,9,10,5,8,4,2,1,6,3,7,9,10,5,8,4,2]))%11]==v[-1].upper()
def luhn(v):
    return sum((n if i%2==0 else (n*2-9 if n>4 else n*2)) for i,n in enumerate(map(int,v[::-1])))%10==0
def valid_ip(v):
    parts = v.split('.')
    return len(parts) == 4 and all(re.fullmatch(r'[0-9]{1,3}', part) and 0 <= int(part) <= 255 for part in parts)
PATTERNS=[
    ('PHONE',r'(?<!\d)(?:1[3-9]\d{9}|0\d{2,3}-?\d{7,8})(?!\d)',None),
    ('ID_CARD',r'(?<![0-9A-Za-z])\d{17}[\dXx](?![0-9A-Za-z])',valid_id),
    ('BANK_CARD',r'(?<!\d)\d{16,19}(?!\d)',luhn),
    ('EMAIL',r'[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}',None),
    ('IP_ADDRESS',r'(?<![\d.])(?:\d{1,3}\.){3}\d{1,3}(?![\d.])',valid_ip),
    ('DATE_TIME',r'\d{4}[年\-/]\d{1,2}[月\-/]\d{1,2}日?(?:\s+\d{1,2}:\d{2}(?::\d{2})?)?',None),
    ('MONEY',r'(?:人民币|RMB|￥|¥)\s?\d+(?:\.\d+)?|\d+(?:\.\d+)?(?:元|万元|亿元|美元|欧元)',None),
    ('LICENSE_PLATE',r'[京津沪渝冀豫云辽黑湘皖鲁新苏浙赣鄂桂甘晋蒙陕吉闽贵粤青藏川宁琼][A-Z][A-Z0-9挂学警港澳领使]{5,6}',None),
    ('WECHAT_ID',r'(?<![A-Za-z0-9_-])[A-Za-z][-_A-Za-z0-9]{5,19}(?![A-Za-z0-9_-])',None),
    ('POSTAL_CODE',r'(?<!\d)[1-9]\d{5}(?!\d)',None),
    ('QQ_NUMBER',r'(?<!\d)[1-9]\d{4,11}(?!\d)',None),
    ('PASSPORT',r'(?<![A-Za-z0-9])[EGPASD][A-Za-z0-9]\d{7}(?![A-Za-z0-9])',None),
    ('MAC_ADDRESS',r'(?<![A-Fa-f0-9])(?:[A-Fa-f0-9]{2}[:-]){5}[A-Fa-f0-9]{2}(?![A-Fa-f0-9])',None),
    ('URL',r'(?<![A-Za-z0-9])https?://[^\s，。；;]+',None),
]
ENTITY_CATALOG = {
    'PERSON':'个人姓名','ORGANIZATION':'组织机构','LOCATION':'地点','GPE':'国家/地区','ADDRESS':'详细地址',
    'PHONE':'电话号码','ID_CARD':'身份证号','BANK_CARD':'银行卡号','EMAIL':'电子邮箱','IP_ADDRESS':'IP 地址',
    'DATE_TIME':'日期时间','MONEY':'金额','LICENSE_PLATE':'车牌号','WECHAT_ID':'微信号','QQ_NUMBER':'数字账号/编号',
    'POSTAL_CODE':'邮政编码','PASSPORT':'护照号','MAC_ADDRESS':'设备地址','URL':'网页地址',
}
BUILTIN_RULES = {typ:{'id':typ.lower(), 'name':ENTITY_CATALOG.get(typ,typ), 'entity_type':typ,
                       'kind':'内置规则', 'description':'系统内置敏感信息识别', 'editable':False}
                 for typ, _, _ in PATTERNS}
def _rule_settings():
    c=conn(); row=c.execute('SELECT value FROM settings WHERE key=?',('builtin_rules',)).fetchone(); c.close()
    try: value=json.loads(row[0]) if row else {}
    except (TypeError, ValueError, json.JSONDecodeError): value={}
    return value if isinstance(value,dict) else {}
def _save_rule_settings(value):
    c=conn(); c.execute('INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value',('builtin_rules',json.dumps(value,ensure_ascii=False))); c.commit(); c.close()
def analyze(text):
    c=conn(); custom=c.execute('SELECT name,kind,pattern,entity_type,replacement FROM rules WHERE enabled=1').fetchall(); c.close(); found=[]; settings=_rule_settings()
    for typ,pat,check in PATTERNS:
        if settings.get(typ.lower(), True) is False: continue
        for m in re.finditer(pat,text,re.IGNORECASE):
            if m.start() == m.end():
                continue
            if typ == 'QQ_NUMBER':
                context = text[max(0, m.start()-8):min(len(text), m.end()+8)]
                if not re.search(r'(?:QQ|扣扣|账号|帐号|联系号码|联系号)', context, re.IGNORECASE):
                    continue
            if check and not check(m.group()): continue
            found.append({'id':uuid.uuid4().hex,'type':typ,'text':m.group(),'start':m.start(),'end':m.end(),'score':.99 if check else .95,'recognizer':'builtin','source':'regex','selected':True,'box_ids':[]})
    for name,kind,pattern,typ,replacement in custom:
        expression = pattern if kind == 'regex' else re.escape(pattern)
        try:
            matches = re.finditer(expression,text,re.IGNORECASE)
        except re.error:
            # Rules are validated on creation; tolerate a legacy/corrupt row
            # so one bad local rule cannot break all analysis requests.
            continue
        for m in matches:
            if m.start() == m.end():
                continue
            found.append({'id':uuid.uuid4().hex,'type':typ,'text':m.group(),'start':m.start(),'end':m.end(),'score':1.0,'recognizer':name or 'custom','source':'custom','replacement':replacement,'selected':True,'box_ids':[]})
    if settings.get('ner_enabled', True):
        found.extend([{**e,'id':uuid.uuid4().hex,'selected':float(e.get('score',0)) >= 0.45,'box_ids':[]} for e in ner_service.analyze(text)])
    # Prefer higher confidence and longer spans when recognizers overlap.
    found.sort(key=lambda x:(-float(x.get('score',0)), -(x['end']-x['start']), x['start']))
    accepted=[]
    for e in found:
        if not any(e['start']<a['end'] and a['start']<e['end'] for a in accepted): accepted.append(e)
        if len(accepted) >= MAX_ENTITIES:
            break
    accepted.sort(key=lambda x:(x['start'], -x['end']))
    # Normalize every detection through Presidio's public result type so all
    # adapters expose one protocol regardless of recognizer implementation.
    try:
        if PresidioRecognizerResult is None:
            return accepted
        normalized = []
        for entity in accepted:
            result = PresidioRecognizerResult(entity_type=entity['type'], start=entity['start'], end=entity['end'], score=entity.get('score', 0.0))
            normalized.append({**entity, 'type': result.entity_type, 'start': result.start, 'end': result.end, 'score': result.score})
        return normalized
    except Exception:
        # Presidio is used here only as the common result protocol.  Its
        # optional device-detection dependencies must never disable the local
        # regex/custom recognizers when unavailable.
        return accepted
def register_task(task_id, kind, original='', masked='', *, source_path='', artifact_name='',
                  original_name=None, source_sha256='', status='created', reversible=False,
                  batch_id='', metadata=None):
    """Register task metadata without putting newly uploaded content in SQLite."""
    if not _valid_task_id(task_id):
        raise ValueError('invalid task id')
    now = datetime.now().timestamp()
    # ``original``/``masked`` are retained only for reading pre-migration legacy
    # rows. New tasks keep content in TTL-managed task files instead of SQLite.
    if original_name is None:
        original_name = original if kind != 'text' else ''
    c=conn()
    c.execute('''INSERT OR REPLACE INTO tasks
        (id,original,masked,created,kind,original_name,source_path,artifact_name,
         source_sha256,status,reversible,batch_id,metadata_json,updated,error)
        VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)''',
        (task_id, original if kind == 'legacy_text' else '',
         masked if kind == 'legacy_text' else '', now, kind, original_name or '',
         source_path or '', artifact_name or '', source_sha256 or '', status,
         int(bool(reversible)), batch_id or '', json.dumps(metadata or {}, ensure_ascii=False), now, ''))
    c.commit(); c.close(); return task_id

def update_task(task_id, masked='', *, status='completed', artifact_name=None,
                source_path=None, reversible=None, error=''):
    if not _valid_task_id(task_id):
        raise ValueError('invalid task id')
    c=conn(); fields=['masked=?','status=?','updated=?','error=?']; values=[masked or '',status,datetime.now().timestamp(),error or '']
    if artifact_name is not None: fields.append('artifact_name=?'); values.append(artifact_name)
    if source_path is not None: fields.append('source_path=?'); values.append(source_path)
    if reversible is not None: fields.append('reversible=?'); values.append(int(bool(reversible)))
    values.append(task_id); c.execute(f"UPDATE tasks SET {', '.join(fields)} WHERE id=?", values); c.commit(); c.close()

def store(original,masked,kind='text'):
    i=uuid.uuid4().hex; d=_task_path(i, create=True); (d/'original.txt').write_text(original,encoding='utf-8'); (d/'masked.txt').write_text(masked,encoding='utf-8')
    register_task(i, kind, source_path='original.txt', artifact_name='masked.txt',
                  original_name='text.txt', source_sha256=_sha256(original.encode('utf-8')),
                  status='completed', metadata={'legacy_text': True})
    return i

class TextIn(BaseModel): text:str=Field(min_length=1,max_length=2_000_000)
class MaskIn(BaseModel):
    text: str = Field(min_length=1, max_length=2_000_000)
    entities: list[dict[str, Any]] | None = Field(default=None, max_length=MAX_ENTITIES)
class RestoreIn(BaseModel):
    task_id: str
    masked_text: str | None = Field(default=None, max_length=2_000_000)
class RestoreFileIn(BaseModel):
    password: str = Field(min_length=1, max_length=256)
class RuleIn(BaseModel):
    name: str = Field(min_length=1, max_length=120)
    kind: str
    pattern: str = Field(min_length=1, max_length=10_000)
    entity_type: str = Field(default='CUSTOM', min_length=1, max_length=80)
    replacement: str = Field(default='__MASKED_CUSTOM__', max_length=500)
class Box(BaseModel):
    id: str = Field(default_factory=lambda:uuid.uuid4().hex, max_length=128)
    page: int = Field(default=1, ge=1, le=10000)
    x: float = Field(ge=0, le=1)
    y: float = Field(ge=0, le=1)
    width: float = Field(gt=0, le=1)
    height: float = Field(gt=0, le=1)
    text: str = Field(default='', max_length=10_000)
    selected: bool = True
    source: str = Field(default='manual', max_length=32)
class ImageMaskIn(BaseModel):
    analysis_id: str
    boxes: list[Box] = Field(default_factory=list, max_length=MAX_IMAGE_BOXES)
    blur_radius: int = Field(default=14,ge=1,le=80)
class DocumentMaskIn(MaskIn):
    analysis_id: str | None = None
    filename: str | None = Field(default=None, max_length=240)
    policies: dict[str, dict[str, Any]] = Field(default_factory=dict, max_length=500)
    boxes: list[dict[str, Any]] | None = Field(default=None, max_length=MAX_IMAGE_BOXES)
    reviewed: bool = True
    reversible: bool = False
    password: str | None = Field(default=None, max_length=256)

def _normalize_text_entities(text: str, entities: Any) -> list[dict[str, Any]]:
    """Validate client supplied text spans before slicing or masking.

    API clients are not trusted, and a malformed offset used to surface as a
    server-side ``TypeError``/``KeyError``.  Returning a stable 422 keeps the
    review protocol predictable and prevents accidental out-of-range edits.
    """
    if not isinstance(text, str) or len(text) > 2_000_000:
        raise HTTPException(422, '文本内容无效或过大')
    if not isinstance(entities, list) or len(entities) > MAX_ENTITIES:
        raise HTTPException(422, '实体数量超过限制')
    normalized: list[dict[str, Any]] = []
    for raw in entities:
        if not isinstance(raw, dict):
            raise HTTPException(422, '实体格式无效')
        try:
            start_raw, end_raw = raw['start'], raw['end']
            if isinstance(start_raw, bool) or isinstance(end_raw, bool):
                raise ValueError
            start, end = int(start_raw), int(end_raw)
        except (KeyError, TypeError, ValueError):
            raise HTTPException(422, '实体区间无效') from None
        target = raw.get('text')
        entity_type = raw.get('type', 'DEFAULT')
        if not isinstance(target, str) or not target or len(target) > 100_000:
            raise HTTPException(422, '实体文本无效')
        if not isinstance(entity_type, str) or not entity_type or len(entity_type) > 80:
            raise HTTPException(422, '实体类型无效')
        if start < 0 or end <= start or end > len(text) or text[start:end] != target:
            raise HTTPException(422, '实体区间与原文不一致')
        selected = raw.get('selected', True)
        if not isinstance(selected, bool):
            raise HTTPException(422, '实体选择状态无效')
        item = dict(raw)
        item.update({'start': start, 'end': end, 'text': target, 'type': entity_type, 'selected': selected})
        normalized.append(item)
    return normalized

def apply_mask(text, entities, policies=None):
    normalized = _normalize_text_entities(text, entities)
    selected = [e for e in normalized if e['selected']]
    # Overlapping spans cannot both be represented in a single text stream;
    # reject them instead of silently masking an arbitrary portion.
    ordered = sorted(selected, key=lambda item: (item['start'], item['end']))
    for previous, current in pairwise(ordered):
        if current['start'] < previous['end']:
            raise HTTPException(422, '实体区间存在重叠')
    try:
        masked, _mapping = apply_entities(text, normalized, policies or policy_store)
    except (KeyError, TypeError, ValueError) as exc:
        raise HTTPException(422, f'实体脱敏失败: {exc}') from exc
    return masked

@app.middleware('http')
async def housekeeping(request,call_next):
    _load_policy_store()
    cleanup(force=False)
    path = request.url.path
    if not _TEST_MODE and path.startswith('/api/') and (path.endswith('/analyze') or path.endswith('/mask')) and not models_ready():
        return _model_gate_response()
    return await call_next(request)
@app.get('/api/health')
def health(): return {'status':'ok','local_only':True,'version':app.version}
@app.get('/api/models/status')
def model_status():
    overall = 'ready' if models_ready() else ('failed' if (ner_service.error or ocr_service.error) else 'starting')
    return {'initialized':models_ready(), 'ner_available':ner_service.available,
            'ocr_available':ocr_service.engine is not None, 'device':'cpu', 'model':MODEL_ID,
            'ner_error':ner_service.error, 'ocr_error':ocr_service.error,
            'status': overall, 'retryable': overall == 'failed'}
@app.post('/api/models/initialize')
def initialize():
    global _active_model_job, _model_state
    with _model_init_lock:
        if _active_model_job and jobs.get(_active_model_job, {}).get('status') == 'running':
            return {'job_id': _active_model_job, 'reused': True}
        i=uuid.uuid4().hex
        _active_model_job = i
        _model_state = 'retrying' if ner_service.error or ocr_service.error else 'starting'
        jobs[i]={'status':'running','progress':5,'message':'正在初始化中文模型','created':time.time()}
        completed_jobs = [
            key for key, value in jobs.items()
            if key != i and value.get('status') != 'running'
        ]
        for old_job in completed_jobs[:-19]:
            jobs.pop(old_job, None)
    def run():
        global _active_model_job
        try:
            jobs[i].update({'progress':20, 'ner_status':'running'})
            ner_service.load(download=True, device='cpu')
            jobs[i].update({'progress':60, 'ner_status':'ready' if ner_service.available else 'error', 'ner_error':ner_service.error})
            ocr_service.load()
            ready = models_ready()
            _model_state = 'ready' if ready else 'failed'
            jobs[i]={'status':'completed' if ready else 'error','progress':100 if ready else 0,
                     'message':'模型初始化完成' if ready else '模型初始化失败',
                     'ner_status':'ready' if ner_service.available else 'error',
                     'ocr_status':'ready' if ocr_service.engine else 'error',
                     'ner_error':ner_service.error, 'ocr_error':ocr_service.error}
        except Exception as exc:
            _model_state = 'failed'
            jobs[i]={'status':'error','progress':0,'message':f'模型初始化异常: {exc}',
                     'ner_status':'ready' if ner_service.available else 'error',
                     'ocr_status':'ready' if ocr_service.engine else 'error', 'ner_error':ner_service.error, 'ocr_error':str(exc)}
        finally:
            with _model_init_lock:
                if _active_model_job == i:
                    _active_model_job = None
    threading.Thread(target=run,daemon=True).start(); return {'job_id':i}
@app.get('/api/models/jobs/{job_id}')
def model_job(job_id): return jobs.get(job_id,{'status':'not_found'})
@app.post('/api/text/analyze')
def text_analyze(v:TextIn): return {'analysis_id':uuid.uuid4().hex,'text':v.text,'entities':analyze(v.text)}
@app.post('/api/text/mask')
def text_mask(v:MaskIn):
    _load_policy_store()
    masked=apply_mask(v.text,v.entities,policy_store)
    return {'task_id':store(v.text,masked),'masked_text':masked}
@app.post('/api/text/unmask')
def text_unmask(v:RestoreIn):
    if not _valid_task_id(v.task_id): raise HTTPException(404,'task not found')
    c=conn(); row=c.execute('SELECT original,masked,source_path,artifact_name FROM tasks WHERE id=?',(v.task_id,)).fetchone(); c.close()
    if not row: raise HTTPException(404,'task not found')
    original, masked, source_path, artifact_name = row
    folder = _task_path(v.task_id)
    if source_path and (folder / source_path).is_file():
        original = (folder / source_path).read_text(encoding='utf-8')
    if artifact_name and (folder / artifact_name).is_file():
        masked = (folder / artifact_name).read_text(encoding='utf-8')
    if not original and not masked: raise HTTPException(409,'该任务没有可恢复的文本')
    return {'task_id':v.task_id,'text':original if v.masked_text is None else v.masked_text.replace(masked,original)}
@app.post('/api/text/restore')
def text_restore(v:RestoreIn): return text_unmask(v)
@app.get('/api/tasks')
def list_tasks(
    limit: int = Query(default=100, ge=1, le=500),
    offset: int = Query(default=0, ge=0),
    status: str | None = Query(default=None, max_length=32),
    kind: str | None = Query(default=None, max_length=32),
):
    clauses: list[str] = []
    values: list[Any] = []
    if status:
        clauses.append('status=?'); values.append(status)
    if kind:
        clauses.append('kind=?'); values.append(kind)
    where = f" WHERE {' AND '.join(clauses)}" if clauses else ''
    # Batch archives are durable artifacts too. Include their parent records in
    # the same history feed so a browser refresh does not make a completed ZIP
    # impossible to find. Child file tasks remain independently downloadable.
    history_sql = '''
        SELECT id,created,kind,original_name,status,artifact_name,restored_name,
               source_sha256,reversible,batch_id,error
        FROM tasks
        UNION ALL
        SELECT id,created,'batch',printf('批量任务（%d 个文件）',total),status,
               archive_name,'','',0,id,error
        FROM batches
    '''
    c=conn()
    total = int(c.execute(f'SELECT COUNT(*) FROM ({history_sql}) AS history{where}', values).fetchone()[0])
    rows=c.execute(
        f'''SELECT id,created,kind,original_name,status,artifact_name,restored_name,
                   source_sha256,reversible,batch_id,error
            FROM ({history_sql}) AS history{where}
            ORDER BY created DESC LIMIT ? OFFSET ?''',
        [*values, limit, offset],
    ).fetchall(); c.close()
    keys=('id','created','kind','filename','status','artifact','restored_artifact','sha256','reversible','batch_id','error')
    return {'tasks':[dict(zip(keys,r)) for r in rows], 'total': total, 'limit': limit, 'offset': offset}
@app.delete('/api/tasks/{task_id}')
def delete_task(task_id):
    folder = _task_path(task_id)
    c=conn()
    row=c.execute('SELECT id,batch_id FROM tasks WHERE id=?',(task_id,)).fetchone()
    batch_row=c.execute('SELECT id,status FROM batches WHERE id=?',(task_id,)).fetchone()
    if not row and not batch_row:
        c.close(); raise HTTPException(404,'task not found')
    active_statuses = {'queued', 'analyzing', 'awaiting_review', 'running'}
    if batch_row and str(batch_row[1]) in active_statuses:
        c.close(); raise HTTPException(409,'请先取消正在处理或等待复核的批次')
    if row and row[1]:
        parent = c.execute('SELECT status FROM batches WHERE id=?',(row[1],)).fetchone()
        if parent and str(parent[0]) in active_statuses:
            c.close(); raise HTTPException(409,'批次仍在处理中，请先取消批次')
    child_ids: list[str] = []
    if batch_row:
        child_ids = [str(item[0]) for item in c.execute('SELECT id FROM tasks WHERE batch_id=?',(task_id,)).fetchall()]
        c.execute('DELETE FROM tasks WHERE batch_id=?',(task_id,))
        c.execute('DELETE FROM batches WHERE id=?',(task_id,))
    if row:
        c.execute('DELETE FROM tasks WHERE id=?',(task_id,))
    c.commit(); c.close()
    shutil.rmtree(folder,ignore_errors=True)
    for child_id in child_ids:
        if _valid_task_id(child_id):
            shutil.rmtree(TASKS / child_id, ignore_errors=True)
    if batch_row:
        with batch_lock:
            batches.pop(task_id, None)
    return {'deleted':task_id, 'children_deleted':len(child_ids)}
@app.get('/api/tasks/{task_id}/artifacts/{name}')
def artifact(task_id,name):
    base=_task_path(task_id); raw_name=str(name); name=Path(raw_name).name
    if raw_name != name or name in {'source.bin','mapping.enc','original.txt'}:
        raise HTTPException(404,'artifact not found')
    # Resolve the exact names recorded for this task.  A suffix-only check
    # would let a caller fetch any attacker-created ``*_masked.*`` file from
    # the private task directory.  Batch archives use the same endpoint but
    # are registered in ``batches`` rather than ``tasks``.
    c=conn()
    row=c.execute('SELECT artifact_name,restored_name,kind FROM tasks WHERE id=?',(task_id,)).fetchone()
    batch_row=c.execute('SELECT archive_name FROM batches WHERE id=?',(task_id,)).fetchone()
    c.close()
    allowed: set[str] = set()
    if row:
        for value in row[:2]:
            if value:
                candidate = Path(str(value)).name
                if candidate == str(value):
                    allowed.add(candidate)
        # Reports are generated only after a file task reaches the masking
        # stage; checking the file below prevents exposing a stale name.
        allowed.add('report.json')
        # Keep compatibility with the first image endpoint migration, where
        # old rows did not persist an artifact name but wrote ``masked.png``.
        if row[2] == 'image':
            allowed.add('masked.png')
    elif batch_row:
        if batch_row[0]:
            candidate = Path(str(batch_row[0])).name
            if candidate == str(batch_row[0]):
                allowed.add(candidate)
        allowed.add('report.json')
    else:
        raise HTTPException(404,'artifact not found')
    if name not in allowed:
        raise HTTPException(404,'artifact not found')
    p=(base/name).resolve()
    if p == base or base not in p.parents or not p.is_file(): raise HTTPException(404,'artifact not found')
    return FileResponse(p, filename=name, content_disposition_type='attachment')
@app.get('/api/rules')
def rules():
    c=conn(); rows=c.execute('SELECT id,name,kind,pattern,entity_type,replacement,enabled FROM rules').fetchall(); c.close(); keys=('id','name','kind','pattern','entity_type','replacement','enabled'); settings=_rule_settings(); builtin=[]
    for typ, meta in BUILTIN_RULES.items(): builtin.append({**meta, 'enabled': settings.get(typ.lower(), True)})
    return {'rules':[dict(zip(keys,r)) for r in rows], 'builtin_rules':builtin, 'ner_enabled':settings.get('ner_enabled',True), 'entity_catalog':ENTITY_CATALOG}
@app.patch('/api/rules/builtin/{rule_id}')
def toggle_builtin(rule_id: str, value: dict[str, bool]):
    typ=next((t for t,m in BUILTIN_RULES.items() if m['id']==rule_id), None)
    if not typ: raise HTTPException(404,'内置规则不存在')
    settings=_rule_settings(); settings[rule_id]=bool(value.get('enabled',True)); _save_rule_settings(settings)
    return {'id':rule_id,'enabled':settings[rule_id]}
@app.patch('/api/rules/model')
def toggle_model(value: dict[str, bool]):
    settings=_rule_settings(); settings['ner_enabled']=bool(value.get('enabled',True)); _save_rule_settings(settings)
    return {'enabled':settings['ner_enabled']}
@app.post('/api/rules')
def add_rule(v:RuleIn):
    if v.kind not in {'word','regex'}: raise HTTPException(422,'kind must be word or regex')
    if v.kind=='regex':
        try: re.compile(v.pattern)
        except re.error as e: raise HTTPException(422,f'invalid regex: {e}')
    i=uuid.uuid4().hex; c=conn(); c.execute('INSERT INTO rules VALUES(?,?,?,?,?,?,1)',(i,v.name,v.kind,v.pattern,v.entity_type,v.replacement)); c.commit(); c.close(); return {'id':i}
@app.delete('/api/rules/{rule_id}')
def remove_rule(rule_id):
    c=conn(); c.execute('DELETE FROM rules WHERE id=?',(rule_id,)); c.commit(); c.close(); return {'deleted':rule_id}
@app.post('/api/image/analyze')
def image_analyze(file:UploadFile=File(...)):
    data=file.file.read(MAX_UPLOAD+1)
    if len(data)>MAX_UPLOAD: raise HTTPException(413,'文件超过 50 MB')
    filename = _safe_filename(file.filename or 'original.png'); ext = extension(filename)
    if ext not in IMAGE_EXTENSIONS: raise HTTPException(415,'不支持的图片格式')
    _validate_upload(data, filename, ext)
    i=uuid.uuid4().hex; folder=_task_path(i, create=True)
    (folder/'source.bin').write_bytes(data)
    try:
        width, height, boxes, entities, warnings = _analyze_image_content(data, folder)
    except Exception:
        shutil.rmtree(folder, ignore_errors=True)
        raise
    payload={'filename':filename,'text':'','entities':entities,'boxes':boxes,
             'image_width':width,'image_height':height,'sha256':_sha256(data),
             'warnings':warnings,'kind':'image'}
    (folder/'analysis.json').write_text(json.dumps(payload,ensure_ascii=False),encoding='utf-8')
    _register_file_task(i,filename,'image',source_sha256=payload['sha256'],status='awaiting_review')
    return {'analysis_id':i,'filename':filename,'image_width':width,'image_height':height,
            'ocr_available':ocr_service.available,'ocr_error':ocr_service.error,
            'boxes':boxes,'entities':entities,'warnings':warnings,'sha256':payload['sha256']}
@app.post('/api/image/mask')
def image_mask(v:ImageMaskIn):
    folder=_task_path(v.analysis_id)
    source = folder / 'source.bin'
    c=conn(); row=c.execute('SELECT original_name,source_sha256 FROM tasks WHERE id=?',(v.analysis_id,)).fetchone(); c.close()
    if not row or not source.is_file():
        raise HTTPException(404,'analysis not found')
    filename=_safe_filename(str(row[0] or 'original.png'))
    checked = _validate_boxes([b.model_dump() for b in v.boxes])
    manifest = _analysis_payload(v.analysis_id)
    # The legacy endpoint predates per-entity policies.  Pass its blur setting
    # through the common file endpoint so source hashing, review selection,
    # reports, and cleanup remain identical to the final workflow.
    result = files_mask(FileMaskIn(
        analysis_id=v.analysis_id,
        filename=filename,
        text=str(manifest.get('text', '')),
        entities=manifest.get('entities', []),
        policies={'DEFAULT': {'image_action': 'blur', 'blur_radius': v.blur_radius}},
        reviewed=True,
        boxes=checked,
    ))
    # Preserve the historical artifact name for callers of /api/image/mask;
    # modern /api/files/mask keeps the original stem in its download name.
    generated = folder / Path(str(result['artifact'])).name
    legacy_name = 'masked.png' if extension(filename) == 'png' else f'masked.{extension(filename)}'
    if generated.is_file() and generated.name != legacy_name:
        generated.replace(folder / legacy_name)
        update_task(v.analysis_id, legacy_name, artifact_name=legacy_name)
        result['artifact'] = f'/api/tasks/{v.analysis_id}/artifacts/{legacy_name}'
    return result
@app.post('/api/document/analyze')
def document_analyze(file:UploadFile=File(...)):
    data=file.file.read(MAX_UPLOAD+1)
    if len(data)>MAX_UPLOAD: raise HTTPException(413,'文件超过 50 MB')
    filename=_safe_filename(file.filename or 'source.bin'); ext=extension(filename); text=''
    if ext not in SUPPORTED: raise HTTPException(415,'不支持的文档格式')
    _validate_upload(data, filename, ext)
    analysis_id=uuid.uuid4().hex; folder=_task_path(analysis_id, create=True); (folder/'source.bin').write_bytes(data)
    boxes: list[dict[str, Any]] = []; warnings: list[dict[str, str]] = []; page_count = None
    try:
        if ext == 'pdf':
            text, entities, boxes, warnings, page_count = _analyze_pdf_content(data, folder)
        elif ext in IMAGE_EXTENSIONS:
            width, height, boxes, entities, warnings = _analyze_image_content(data, folder)
            text = '\n'.join(str(box.get('text', '')) for box in boxes)
        else:
            text = extract_text(data, filename)
            entities = analyze(text) if text else []
    except Exception:
        shutil.rmtree(folder, ignore_errors=True)
        raise
    _register_file_task(analysis_id,filename,'document',source_sha256=_sha256(data),status='awaiting_review')
    content=[{'page':1,'type':'text','text':text}]
    (folder/'result.md').write_text(text,encoding='utf-8')
    (folder/'content.json').write_text(json.dumps(content,ensure_ascii=False),encoding='utf-8')
    (folder/'entities.json').write_text(json.dumps(entities,ensure_ascii=False),encoding='utf-8')
    manifest={'filename':filename,'text':text,'entities':entities,'boxes':boxes,'warnings':warnings,'sha256':_sha256(data),'kind':ext}
    if page_count is not None: manifest['page_count']=page_count
    (folder/'analysis.json').write_text(json.dumps(manifest,ensure_ascii=False),encoding='utf-8')
    result={'analysis_id':analysis_id,'filename':filename,'text':text,'entities':entities,
            'boxes':boxes,'warnings':warnings,'content':content,'sha256':manifest['sha256'],
            # Extracted text and analysis snapshots are inline review data;
            # they are not downloadable artifacts because they contain the
            # original sensitive values.
            'artifacts':[]}
    if page_count is not None: result['page_count']=page_count
    return result
@app.post('/api/document/mask')
def document_mask(v:DocumentMaskIn):
    if not v.analysis_id:
        if v.reversible:
            raise HTTPException(422, '可逆文件模式需要先通过文件分析接口创建任务')
        result = text_mask(v)
        c=conn(); c.execute('UPDATE tasks SET kind=? WHERE id=?',('document',result['task_id'])); c.commit(); c.close()
        return result
    # Keep the legacy route as a compatibility shim, but run exactly the same
    # validation, policy, report, and reversible-file path as the unified API.
    # This prevents the old document tab/API from silently producing a result
    # that cannot be restored or that differs from ``/api/files/mask``.
    filename = v.filename
    if not filename:
        c = conn()
        row = c.execute('SELECT original_name FROM tasks WHERE id=?', (v.analysis_id,)).fetchone()
        c.close()
        if not row:
            raise HTTPException(404, 'analysis not found')
        filename = str(row[0] or '')
    return files_mask(FileMaskIn(
        analysis_id=v.analysis_id,
        filename=filename,
        text=v.text,
        entities=v.entities,
        policies=v.policies,
        reviewed=v.reviewed,
        reversible=v.reversible,
        password=v.password,
        boxes=v.boxes,
    ))

# Unified final-version file and batch APIs. The legacy routes above remain available.
class FileMaskIn(BaseModel):
    analysis_id: str
    filename: str = Field(min_length=1, max_length=240)
    text: str = Field(default='', max_length=2_000_000)
    entities: list[dict[str, Any]] | None = Field(default=None, max_length=MAX_ENTITIES)
    policies: dict[str, dict[str, Any]] = Field(default_factory=dict, max_length=500)
    reviewed: bool = False
    reversible: bool = False
    password: str | None = Field(default=None, max_length=256)
    boxes: list[dict[str, Any]] | None = Field(default=None, max_length=MAX_IMAGE_BOXES)
    image_override: bool = False

class BatchMaskIn(BaseModel):
    policies: dict[str, dict[str, Any]] = Field(default_factory=dict, max_length=500)
    reviewed: bool = False
    reversible: bool = False
    password: str | None = Field(default=None, max_length=256)
    items: list[dict[str, Any]] | None = Field(default=None, max_length=MAX_BATCH_FILES)

def _task_folder(task_id: str, *, create: bool = False) -> Path:
    """Resolve a task directory, creating it only for an explicit write.

    Read endpoints must not create directories for random or expired task
    identifiers.  Keeping the switch here makes accidental writes during
    validation visible at the call site.
    """
    return _task_path(task_id, create=create)

def _validate_reversible_password(password: str | None) -> str:
    if not password:
        raise HTTPException(422, '可逆模式需要口令')
    try:
        validate_password(password)
    except ValueError as exc:
        raise HTTPException(422, str(exc)) from exc
    return password

def _register_file_task(task_id: str, filename: str, kind: str, *, source_sha256='',
                        source_path='source.bin', status='analyzing', batch_id='', metadata=None) -> None:
    register_task(task_id, kind, original_name=_safe_filename(filename), source_path=source_path,
                  source_sha256=source_sha256, status=status, batch_id=batch_id, metadata=metadata)

def _analysis_payload(task_id: str) -> dict[str, Any]:
    folder = _task_path(task_id)
    path = folder / 'analysis.json'
    if not path.is_file():
        raise HTTPException(404, 'analysis not found')
    try:
        payload = json.loads(path.read_text(encoding='utf-8'))
    except (OSError, json.JSONDecodeError) as exc:
        raise HTTPException(409, '分析结果已损坏') from exc
    if not isinstance(payload, dict):
        raise HTTPException(409, '分析结果格式无效')
    return payload


_PRIVATE_ANALYSIS_FILES = {
    'analysis.json', 'result.md', 'content.json', 'entities.json',
    '.ocr-input.png',
}


def _remove_private_analysis(folder: Path, *, remove_source: bool = False) -> None:
    """Delete intermediate files that can contain the original values."""
    for name in _PRIVATE_ANALYSIS_FILES:
        (folder / name).unlink(missing_ok=True)
    if remove_source:
        (folder / 'source.bin').unlink(missing_ok=True)


def _safe_report(
    filename: str,
    text: str,
    entities: list[dict[str, Any]],
    policies: dict[str, dict[str, Any]] | None,
    *,
    reversible: bool,
    source_sha256: str,
    boxes: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    """Build an audit report without copying recognized values into it."""
    safe_entities: list[dict[str, Any]] = []
    type_counts: dict[str, int] = {}
    selected_count = 0
    for entity in entities:
        if not isinstance(entity, dict):
            continue
        entity_type = str(entity.get('type', 'DEFAULT'))
        type_counts[entity_type] = type_counts.get(entity_type, 0) + 1
        selected = bool(entity.get('selected', True))
        selected_count += int(selected)
        item: dict[str, Any] = {
            'type': entity_type,
            'start': entity.get('start', 0),
            'end': entity.get('end', 0),
            'score': entity.get('score', 0),
            'selected': selected,
        }
        for key in ('page', 'bbox', 'box_ids', 'source', 'recognizer'):
            if key in entity:
                item[key] = entity[key]
        safe_entities.append(item)
    safe_policies: dict[str, dict[str, Any]] = {}
    for entity_type, policy in (policies or {}).items():
        if not isinstance(policy, dict):
            continue
        safe_policies[str(entity_type)] = {
            key: policy[key]
            for key in ('text_action', 'image_action', 'color', 'blur_radius', 'pixel_size')
            if key in policy
        }
    return {
        'filename': _safe_filename(filename),
        'text_length': len(text),
        'entity_count': len(safe_entities),
        'selected_entity_count': selected_count,
        'entity_types': dict(sorted(type_counts.items())),
        'entities': safe_entities,
        'box_count': len(boxes or []),
        'selected_box_count': sum(
            1 for box in (boxes or [])
            if isinstance(box, dict) and bool(box.get('selected', True))
        ),
        'policies': safe_policies,
        'reversible': bool(reversible),
        'sha256': source_sha256,
    }


def _sanitize_batch_items(items: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Retain batch outcome metadata while removing original text values."""
    sanitized: list[dict[str, Any]] = []
    for raw in items:
        item = dict(raw) if isinstance(raw, dict) else {'status': 'failed', 'error': '批次项目格式无效'}
        item.pop('text', None)
        for collection in ('entities', 'boxes'):
            values = item.get(collection)
            if not isinstance(values, list):
                continue
            clean_values = []
            for value in values:
                if not isinstance(value, dict):
                    continue
                clean = dict(value)
                clean.pop('text', None)
                clean.pop('replacement', None)
                clean_values.append(clean)
            item[collection] = clean_values
        sanitized.append(item)
    return sanitized

def _map_ocr_result(raw_boxes: list[dict[str, Any]], width: int, height: int, *, page: int,
                    text_offset: int = 0) -> tuple[list[dict[str, Any]], list[dict[str, Any]], str, list[dict[str, str]]]:
    """Convert Paddle coordinates to the shared normalized entity protocol."""
    boxes: list[dict[str, Any]] = []
    entities: list[dict[str, Any]] = []
    lines: list[str] = []
    warnings: list[dict[str, str]] = []
    cursor = text_offset
    for raw in raw_boxes:
        if not isinstance(raw, dict):
            continue
        try:
            x1, y1, x2, y2 = (float(raw[key]) for key in ('x1', 'y1', 'x2', 'y2'))
            score = float(raw.get('score', 0))
        except (KeyError, TypeError, ValueError):
            continue
        if not all(math.isfinite(value) for value in (x1, y1, x2, y2, score)):
            continue
        left, top = max(0.0, min(x1, x2)), max(0.0, min(y1, y2))
        right, bottom = min(float(width), max(x1, x2)), min(float(height), max(y1, y2))
        if right <= left or bottom <= top:
            continue
        text = str(raw.get('text', ''))[:100_000]
        line_start = cursor
        line_end = line_start + len(text)
        cursor = line_end + 1
        lines.append(text)
        box_id = uuid.uuid4().hex
        bbox = {'x': left / width, 'y': top / height, 'width': (right - left) / width, 'height': (bottom - top) / height}
        polygon = raw.get('polygon')
        if not isinstance(polygon, list) or len(polygon) < 4:
            polygon = [[left, top], [right, top], [right, bottom], [left, bottom]]
        else:
            polygon = [[float(point[0]) / width, float(point[1]) / height] for point in polygon if isinstance(point, (list, tuple)) and len(point) >= 2]
        box = {'id': box_id, 'type': 'OCR', 'text': text, 'score': max(0.0, min(1.0, score)),
               'start': line_start, 'end': line_end, 'bbox': bbox, 'page': page, 'selected': True,
               'source': 'ocr', 'entity_ids': [], 'polygon': polygon}
        boxes.append(box)
        if not text:
            continue
        try:
            line_entities = analyze(text)
        except Exception as exc:
            warnings.append({'code': 'ENTITY_FAILED', 'message': f'第 {page} 页实体识别失败: {exc}'})
            continue
        for entity in line_entities:
            entity = dict(entity)
            entity['bbox'] = dict(bbox)
            entity['line_bbox'] = dict(bbox)
            entity['line_polygon'] = list(polygon)
            entity['char_start'] = int(entity.get('start', 0))
            entity['char_end'] = int(entity.get('end', 0))
            entity['page'] = page
            entity['box_ids'] = [box_id]
            entity['start'] = line_start + int(entity.get('start', 0))
            entity['end'] = line_start + int(entity.get('end', 0))
            local_start = max(0, min(len(text), int(entity['char_start'])))
            local_end = max(local_start, min(len(text), int(entity['char_end'])))
            if local_end > local_start and text:
                # Estimate a precise horizontal sub-region from character advances.
                # PaddleOCR exposes line geometry but not per-character boxes.
                advances = [1.0 if ord(ch) > 0x7f else 0.58 for ch in text]
                total = max(sum(advances), 1.0)
                start_ratio = sum(advances[:local_start]) / total
                end_ratio = sum(advances[:local_end]) / total
                entity['bbox'] = {
                    'x': bbox['x'] + bbox['width'] * start_ratio,
                    'y': bbox['y'],
                    'width': max(0.001, bbox['width'] * (end_ratio - start_ratio)),
                    'height': bbox['height'],
                }
            entity['id'] = str(entity.get('id') or uuid.uuid4().hex)
            box['entity_ids'].append(entity['id'])
            entities.append(entity)
        # Only lines containing a recognized sensitive entity are selected by
        # default.  Other OCR lines remain available for an explicit manual
        # review without being accidentally covered in batch processing.
        box['selected'] = bool(box['entity_ids'])
    return boxes, entities, '\n'.join(lines), warnings


def _analyze_image_content(data: bytes, folder: Path) -> tuple[int, int, list[dict[str, Any]], list[dict[str, Any]], list[dict[str, str]]]:
    """Run OCR and map every OCR line to normalized image coordinates."""
    try:
        with Image.open(io.BytesIO(data)) as opened:
            oriented = ImageOps.exif_transpose(opened).convert('RGB')
            width, height = oriented.size
            if width <= 0 or height <= 0 or width * height > MAX_IMAGE_PIXELS:
                raise HTTPException(413, f'图片像素超过限制（最多 {MAX_IMAGE_PIXELS}）')
            ocr_input = folder / '.ocr-input.png'
            oriented.save(ocr_input, format='PNG')
    except HTTPException:
        raise
    except Exception as exc:
        raise HTTPException(415, f'无法读取图片: {exc}') from exc
    try:
        if not ocr_service.available:
            return width, height, [], [], [{'code': 'OCR_UNAVAILABLE', 'message': 'OCR 模型尚未初始化'}]
        try:
            raw_boxes = ocr_service.analyze(str(ocr_input))
        except Exception as exc:
            return width, height, [], [], [{'code': 'OCR_FAILED', 'message': str(exc)}]
        boxes, entities, _text, warnings = _map_ocr_result(raw_boxes, width, height, page=1)
    finally:
        ocr_input.unlink(missing_ok=True)
    return width, height, boxes, entities, warnings


def _pdf_page_needs_ocr(page: Any, native_text: str) -> bool:
    if not re.sub(r'\s+', '', native_text):
        return True
    # A scan can contain a small native page number or header. Detect a large
    # raster covering the page instead of assuming any extracted text means
    # the whole page is searchable.
    if len(re.sub(r'\s+', '', native_text)) >= 80:
        return False
    page_area = max(1.0, float(page.rect.width * page.rect.height))
    try:
        for info in page.get_image_info():
            rect = info.get('bbox') if isinstance(info, dict) else None
            if rect and max(0.0, float(rect[2] - rect[0])) * max(0.0, float(rect[3] - rect[1])) / page_area >= 0.35:
                return True
    except (AttributeError, TypeError, ValueError):
        pass
    return False


def _analyze_pdf_content(data: bytes, folder: Path) -> tuple[str, list[dict[str, Any]], list[dict[str, Any]], list[dict[str, str]], int]:
    """Analyze searchable PDF text and OCR raster-only pages."""
    import fitz

    document = fitz.open(stream=data, filetype='pdf')
    page_texts: list[str] = []
    entities: list[dict[str, Any]] = []
    boxes: list[dict[str, Any]] = []
    warnings: list[dict[str, str]] = []
    ocr_pages = 0
    global_cursor = 0
    page_count = document.page_count
    try:
        for page_index, page in enumerate(document):
            page_number = page_index + 1
            native_text = page.get_text()
            logical_text = native_text
            for entity in analyze(native_text) if native_text else []:
                entity = dict(entity)
                entity.update({'page': page_number,
                               'start': int(entity.get('start', 0)) + global_cursor,
                               'end': int(entity.get('end', 0)) + global_cursor})
                entities.append(entity)
            if not _pdf_page_needs_ocr(page, native_text):
                page_texts.append(logical_text)
                global_cursor += len(logical_text) + 1
                continue
            if page_number > MAX_PDF_OCR_PAGES:
                if not any(item.get('code') == 'PDF_OCR_PAGE_LIMIT' for item in warnings):
                    warnings.append({'code': 'PDF_OCR_PAGE_LIMIT', 'message': f'PDF OCR 最多处理前 {MAX_PDF_OCR_PAGES} 页'})
                page_texts.append(logical_text)
                global_cursor += len(logical_text) + 1
                continue
            if not ocr_service.enabled:
                if not any(item.get('code') == 'OCR_UNAVAILABLE' for item in warnings):
                    warnings.append({'code': 'OCR_UNAVAILABLE', 'message': '检测到扫描页，请先在“本地模型”中初始化 OCR'})
                page_texts.append(logical_text)
                global_cursor += len(logical_text) + 1
                continue
            base_pixels = max(1.0, float(page.rect.width * page.rect.height))
            scale = min(2.0, math.sqrt(MAX_PDF_OCR_PIXELS / base_pixels))
            scale = max(0.25, scale)
            pixmap = page.get_pixmap(matrix=fitz.Matrix(scale, scale), alpha=False, colorspace=fitz.csRGB)
            ocr_input = folder / f'.pdf-ocr-{page_number}.png'
            try:
                pixmap.save(ocr_input)
                raw_boxes = ocr_service.analyze(str(ocr_input))
            except Exception as exc:
                warnings.append({'code': 'PDF_OCR_FAILED', 'message': f'第 {page_number} 页 OCR 失败: {exc}'})
                page_texts.append(logical_text)
                global_cursor += len(logical_text) + 1
                continue
            finally:
                ocr_input.unlink(missing_ok=True)
            base_offset = global_cursor + len(native_text) + (1 if native_text and raw_boxes else 0)
            page_boxes, page_entities, ocr_text, page_warnings = _map_ocr_result(
                raw_boxes, pixmap.width, pixmap.height, page=page_number, text_offset=base_offset,
            )
            boxes.extend(page_boxes)
            entities.extend(page_entities)
            warnings.extend(page_warnings)
            if ocr_text:
                logical_text = native_text + ('\n' if native_text else '') + ocr_text
                ocr_pages += 1
            else:
                warnings.append({'code': 'PDF_OCR_EMPTY', 'message': f'第 {page_number} 页未识别到文字'})
            page_texts.append(logical_text)
            global_cursor += len(logical_text) + 1
        if ocr_pages:
            warnings.insert(0, {'code': 'PDF_OCR_USED', 'message': f'已对 {ocr_pages} 个扫描页执行本地 OCR'})
    finally:
        document.close()
    if len(entities) > MAX_ENTITIES:
        entities = entities[:MAX_ENTITIES]
        warnings.append({'code': 'ENTITY_LIMIT', 'message': f'实体过多，仅保留前 {MAX_ENTITIES} 个'})
    if len(boxes) > MAX_IMAGE_BOXES:
        boxes = boxes[:MAX_IMAGE_BOXES]
        warnings.append({'code': 'BOX_LIMIT', 'message': f'OCR 文字框过多，仅保留前 {MAX_IMAGE_BOXES} 个'})
    return '\n'.join(page_texts), entities, boxes, warnings, page_count


def _require_complete_pdf_review(manifest: dict[str, Any], boxes: list[dict[str, Any]]) -> None:
    """Reject a scanned PDF result when pages could not be inspected.

    Returning a downloadable copy while OCR was unavailable or failed gives a
    false success signal even though rasterized source text is still visible.
    A user may retry after model initialization; successfully reviewed OCR
    boxes remain eligible for masking.
    """
    warnings = manifest.get('warnings', [])
    blocking = {'OCR_UNAVAILABLE', 'PDF_OCR_FAILED', 'PDF_OCR_EMPTY', 'PDF_OCR_PAGE_LIMIT'}
    codes = {
        str(item.get('code'))
        for item in warnings
        if isinstance(item, dict) and item.get('code')
    }
    if codes & blocking:
        raise HTTPException(409, '扫描 PDF 尚未完成 OCR，请初始化 OCR 模型并重新分析后再脱敏')
    if any(isinstance(box, dict) and box.get('source') == 'ocr' for box in manifest.get('boxes', [])) and not boxes:
        raise HTTPException(409, '扫描 PDF 的 OCR 区域缺失，请重新分析并复核')

def _entity_key(entity: dict[str, Any]) -> tuple[Any, ...]:
    if not isinstance(entity, dict):
        raise HTTPException(422, '实体格式无效')
    bbox = entity.get('bbox') or {}
    if not isinstance(bbox, dict):
        raise HTTPException(422, '实体框格式无效')
    try:
        start = entity.get('start', 0)
        end = entity.get('end', 0)
        page = entity.get('page', 1)
        if any(isinstance(value, bool) for value in (start, end, page)):
            raise ValueError
        start, end, page = int(start), int(end), int(page)
        coords = tuple(round(float(bbox.get(key, 0)), 8) for key in ('x', 'y', 'width', 'height'))
    except (TypeError, ValueError, OverflowError):
        raise HTTPException(422, '实体区间或坐标无效') from None
    if not all(value == value and abs(value) != float('inf') for value in coords):
        raise HTTPException(422, '实体坐标无效')
    return (str(entity.get('type', '')), str(entity.get('text', '')), start, end, *coords, page)

def _review_entities(server_entities: list[dict[str, Any]], submitted: list[dict[str, Any]], *, allow_new=False) -> list[dict[str, Any]]:
    """Accept only selection/box edits over the server analysis snapshot."""
    if not isinstance(server_entities, list) or len(server_entities) > MAX_ENTITIES:
        raise HTTPException(409, '分析实体数量或格式无效')
    if not isinstance(submitted, list) or len(submitted) > MAX_ENTITIES:
        raise HTTPException(422, '提交的实体数量超过限制')
    if any(not isinstance(item, dict) for item in submitted):
        raise HTTPException(422, '实体格式无效')
    by_id = {str(item.get('id')): item for item in submitted if item.get('id')}
    by_key = {_entity_key(item): item for item in submitted}
    result=[]
    for original in server_entities:
        if not isinstance(original, dict):
            raise HTTPException(409, '分析实体格式无效')
        candidate = by_id.get(str(original.get('id'))) or by_key.get(_entity_key(original))
        merged = dict(original)
        if candidate is not None:
            immutable = ('type', 'text', 'start', 'end')
            if any(str(candidate.get(k, '')) != str(original.get(k, '')) for k in immutable):
                raise HTTPException(422, '实体内容与分析结果不一致')
            selected = candidate.get('selected', True)
            if not isinstance(selected, bool):
                raise HTTPException(422, '实体选择状态无效')
            merged['selected'] = selected
            if original.get('bbox') and candidate.get('bbox'):
                checked = _validate_boxes([candidate['bbox']])[0]
                merged['bbox'] = {key: checked[key] for key in ('x', 'y', 'width', 'height')}
        else:
            merged['selected'] = False
        result.append(merged)
    if not allow_new:
        known_ids = {str(item.get('id')) for item in server_entities}
        known_keys = {_entity_key(item) for item in server_entities}
        for candidate in submitted:
            if str(candidate.get('id')) not in known_ids and _entity_key(candidate) not in known_keys:
                raise HTTPException(422, '提交了未来源于分析结果的实体')
    else:
        known_ids = {str(x.get('id')) for x in server_entities}
        known_keys = {_entity_key(x) for x in server_entities}
        for candidate in submitted:
            if str(candidate.get('id')) not in known_ids and _entity_key(candidate) not in known_keys:
                selected = candidate.get('selected', True)
                if not isinstance(selected, bool):
                    raise HTTPException(422, '实体选择状态无效')
                if selected:
                    if not isinstance(candidate.get('bbox'), dict):
                        raise HTTPException(422, '新增图片实体必须包含坐标框')
                    checked = _validate_boxes([candidate['bbox']])[0]
                    item = dict(candidate)
                    item['bbox'] = {key: checked[key] for key in ('x', 'y', 'width', 'height')}
                    result.append(item)
    return result

def _validate_boxes(boxes: list[dict[str, Any]]) -> list[dict[str, Any]]:
    if not isinstance(boxes, list) or len(boxes) > MAX_IMAGE_BOXES:
        raise HTTPException(422, '图片框数量超过限制')
    validated=[]
    for box in boxes:
        if not isinstance(box, dict):
            raise HTTPException(422, '图片框格式无效')
        selected = box.get('selected', True)
        if not isinstance(selected, bool):
            raise HTTPException(422, '图片框选择状态无效')
        page_raw = box.get('page', 1)
        if isinstance(page_raw, bool):
            raise HTTPException(422, '图片框页码无效')
        try:
            page = int(page_raw)
        except (TypeError, ValueError):
            raise HTTPException(422, '图片框页码无效') from None
        if page < 1 or page > 10000:
            raise HTTPException(422, '图片框页码超出范围')
        # The public OCR protocol nests normalized coordinates under ``bbox``;
        # older clients and hand-authored requests send them at the top level.
        # Accept both forms and return a canonical copy so the image adapter
        # receives the same shape regardless of the client.
        coordinates = box.get('bbox') if isinstance(box.get('bbox'), dict) else box
        try:
            raw_values = tuple(coordinates.get(k, 0) for k in ('x','y','width','height'))
            if any(isinstance(value, bool) for value in raw_values):
                raise ValueError
            x,y,w,h = (float(value) for value in raw_values)
        except (TypeError, ValueError) as exc:
            raise HTTPException(422, '图片框坐标无效') from exc
        if not all(math.isfinite(value) for value in (x, y, w, h)) or w <= 0 or h <= 0 or x < 0 or y < 0 or x >= 1 or y >= 1:
            raise HTTPException(422, '图片框坐标超出范围')
        if x + w > 1 or y + h > 1:
            raise HTTPException(422, '图片框坐标超出范围')
        item=dict(box)
        item.update({'x':x,'y':y,'width':w,'height':h,
                     'bbox': {'x':x,'y':y,'width':w,'height':h},
                     'page': page, 'selected': selected})
        validated.append(item)
    return validated


def _selection_region_key(value: dict[str, Any]) -> tuple[Any, ...] | None:
    """Return a stable page/geometry key for entity-to-box association."""
    if not isinstance(value, dict):
        return None
    bbox = value.get('bbox') if isinstance(value.get('bbox'), dict) else value
    try:
        page_raw = value.get('page', 1)
        if isinstance(page_raw, bool):
            return None
        page = int(page_raw)
        coords = tuple(round(float(bbox.get(key, 0)), 8) for key in ('x', 'y', 'width', 'height'))
    except (TypeError, ValueError, OverflowError):
        return None
    if not all(math.isfinite(item) for item in coords):
        return None
    return (page, *coords)


def _synchronize_review_selection(
    entities: list[dict[str, Any]],
    boxes: list[dict[str, Any]],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Make OCR entity and parent-region selections agree.

    OCR produces a line box plus zero or more sensitive entities inside it.
    Treating those as independent masks can re-mask a deselected entity with
    the generic OCR policy, or report a selected box even though the rendered
    artifact was left untouched.  Manual regions have no entity association
    and intentionally remain independently selectable.
    """
    normalized_entities = [dict(item) for item in entities if isinstance(item, dict)]
    normalized_boxes = [dict(item) for item in boxes if isinstance(item, dict)]
    entity_by_id = {
        str(entity.get('id')): index
        for index, entity in enumerate(normalized_entities)
        if entity.get('id')
    }
    box_by_id = {
        str(box.get('id')): index
        for index, box in enumerate(normalized_boxes)
        if box.get('id')
    }
    links: dict[int, set[int]] = {}

    def link(box_index: int, entity_index: int) -> None:
        if 0 <= box_index < len(normalized_boxes) and 0 <= entity_index < len(normalized_entities):
            links.setdefault(box_index, set()).add(entity_index)

    for box_index, box in enumerate(normalized_boxes):
        raw_ids = box.get('entity_ids', [])
        if isinstance(raw_ids, (list, tuple, set)):
            for raw_id in raw_ids:
                entity_index = entity_by_id.get(str(raw_id))
                if entity_index is not None:
                    link(box_index, entity_index)
    for entity_index, entity in enumerate(normalized_entities):
        raw_ids = entity.get('box_ids', [])
        if isinstance(raw_ids, (list, tuple, set)):
            for raw_id in raw_ids:
                box_index = box_by_id.get(str(raw_id))
                if box_index is not None:
                    link(box_index, entity_index)

    # A few older clients omit the relationship IDs.  Match OCR geometry as a
    # compatibility fallback, while leaving explicitly drawn manual regions
    # independent even when they happen to overlap an entity.
    for box_index, box in enumerate(normalized_boxes):
        if box_index in links or str(box.get('source', '')).lower() == 'manual':
            continue
        box_key = _selection_region_key(box)
        if box_key is None:
            continue
        for entity_index, entity in enumerate(normalized_entities):
            if _selection_region_key(entity) == box_key:
                link(box_index, entity_index)

    for box_index, entity_indexes in links.items():
        box = normalized_boxes[box_index]
        box_selected = bool(box.get('selected', True))
        # A region containing at least one selected entity is effective.  If a
        # reviewer deselects every entity in an OCR line, the generic OCR box
        # must not mask it again.
        any_entity_selected = any(
            bool(normalized_entities[index].get('selected', True))
            for index in entity_indexes
        )
        effective = box_selected and any_entity_selected
        box['selected'] = effective
        for entity_index in entity_indexes:
            normalized_entities[entity_index]['selected'] = bool(
                normalized_entities[entity_index].get('selected', True)
            ) and effective
    return normalized_entities, normalized_boxes


def _validate_policy_map(value: Any) -> dict[str, dict[str, Any]]:
    """Validate request-level masking policies before adapters consume them."""
    if value is None:
        return {}
    if not isinstance(value, dict) or len(value) > 500:
        raise HTTPException(422, '策略格式或数量无效')
    allowed_text = {'keep', 'token', 'replace', 'mask', 'hash', 'pseudonym'}
    allowed_image = {'keep', 'blur', 'pixelate', 'solid', 'text'}
    result: dict[str, dict[str, Any]] = {}
    for entity_type, policy in value.items():
        if not isinstance(entity_type, str) or not entity_type or len(entity_type) > 80:
            raise HTTPException(422, '策略格式无效')
        if not isinstance(policy, dict) or len(policy) > 32:
            raise HTTPException(422, f'策略格式无效: {entity_type}')
        text_action = policy.get('text_action', 'token')
        image_action = policy.get('image_action', 'solid')
        if text_action not in allowed_text:
            raise HTTPException(422, f'不支持的文本策略: {entity_type}')
        if image_action not in allowed_image:
            raise HTTPException(422, f'不支持的图片策略: {entity_type}')
        replacement = policy.get('replacement')
        if replacement is not None and (not isinstance(replacement, str) or len(replacement) > 500):
            raise HTTPException(422, f'策略替换文本无效: {entity_type}')
        for key, lower, upper in (('blur_radius', 1, 80), ('pixel_size', 2, 40)):
            if key not in policy:
                continue
            numeric = policy[key]
            if isinstance(numeric, bool):
                raise HTTPException(422, f'策略数值无效: {entity_type}.{key}')
            try:
                numeric = int(numeric)
            except (TypeError, ValueError):
                raise HTTPException(422, f'策略数值无效: {entity_type}.{key}') from None
            if numeric < lower or numeric > upper:
                raise HTTPException(422, f'策略数值超出范围: {entity_type}.{key}')
        color = policy.get('color')
        if color is not None and (not isinstance(color, str) or len(color) > 32):
            raise HTTPException(422, f'策略颜色无效: {entity_type}')
        if 'show_replacement' in policy and not isinstance(policy['show_replacement'], bool):
            raise HTTPException(422, f'替换文字开关无效: {entity_type}')
        result[entity_type] = dict(policy)
    return result


# Initialize persisted overrides after the validator is defined.  This runs
# once at import time and is intentionally tolerant of a damaged local DB.
_load_policy_store()

@app.get('/api/policies')
def get_policies():
    _load_policy_store()
    return {'policies': policy_store, 'catalog': list(policy_store.keys()), 'defaults_version': POLICY_VERSION}

@app.post('/api/policies/reset')
def reset_policies():
    global policy_store
    policy_store = {key: dict(value) for key, value in DEFAULT_POLICIES.items()}
    _persist_policy_store()
    return {'policies': policy_store, 'defaults_version': POLICY_VERSION}

@app.put('/api/policies')
def put_policies(value: dict[str, dict[str, Any]]):
    _load_policy_store()
    policy_store.update(_validate_policy_map(value))
    _persist_policy_store()
    return {'policies': policy_store}

@app.post('/api/files/analyze')
def files_analyze(file: UploadFile = File(...)):
    data = file.file.read(MAX_UPLOAD + 1)
    if len(data) > MAX_UPLOAD:
        raise HTTPException(413, '文件超过 50 MB')
    filename = _safe_filename(file.filename or 'upload.bin')
    ext = extension(filename)
    if ext not in SUPPORTED:
        raise HTTPException(415, f'不支持的文件格式: .{ext}')
    _validate_upload(data, filename, ext)
    task_id = uuid.uuid4().hex
    folder = _task_folder(task_id, create=True)
    text = ''
    entities: list[dict[str, Any]] = []
    boxes: list[dict[str, Any]] = []
    warnings: list[dict[str, str]] = []
    pdf_page_count = None
    image_width = image_height = None
    registered = False
    try:
        (folder / 'source.bin').write_bytes(data)
        if ext == 'pdf':
            text, entities, boxes, warnings, pdf_page_count = _analyze_pdf_content(data, folder)
        elif ext in IMAGE_EXTENSIONS:
            image_width, image_height, boxes, ocr_entities, warnings = _analyze_image_content(data, folder)
            # Keep OCR text in the review payload as well as the normalized
            # boxes. This makes an image analysis usable by clients that render
            # the common text/entity review surface instead of a canvas.
            text = '\n'.join(str(box.get('text', '')) for box in boxes if box.get('text'))
            entities = ocr_entities
        else:
            text = extract_text(data, filename)
            entities = analyze(text) if text else []

        manifest = content_manifest(filename, text, entities) | {
            'text': text, 'sha256': _sha256(data), 'kind': ext, 'warnings': warnings,
            'boxes': boxes,
        }
        if pdf_page_count is not None:
            manifest['page_count'] = pdf_page_count
        (folder / 'analysis.json').write_text(json.dumps(manifest, ensure_ascii=False), encoding='utf-8')
        _register_file_task(task_id, filename, ext, source_sha256=manifest['sha256'], status='awaiting_review', metadata={'manifest_version': 1})
        registered = True
    except HTTPException:
        shutil.rmtree(folder, ignore_errors=True)
        if registered:
            c = conn()
            c.execute('DELETE FROM tasks WHERE id=?', (task_id,))
            c.commit()
            c.close()
        raise
    except Exception as exc:
        shutil.rmtree(folder, ignore_errors=True)
        if registered:
            c = conn()
            c.execute('DELETE FROM tasks WHERE id=?', (task_id,))
            c.commit()
            c.close()
        raise HTTPException(415, f'文件解析失败: {exc}') from exc
    result = {'analysis_id': task_id, 'filename': filename, 'kind': ext, 'text': text,
              'entities': entities, 'warnings': warnings, 'sha256': manifest['sha256'],
              # The analysis snapshot remains private and is consumed by the
              # masking endpoint from disk after review.
              'artifacts': []}
    if image_width is not None and image_height is not None:
        result.update({'image_width': image_width, 'image_height': image_height, 'boxes': boxes})
    if pdf_page_count is not None:
        result.update({'page_count': pdf_page_count, 'boxes': boxes})
    return result

@app.post('/api/files/mask')
def files_mask(value: FileMaskIn):
    # `reviewed` is retained for client compatibility. The execute action is
    # the confirmation; individual selected flags remain authoritative.
    folder = _task_folder(value.analysis_id)
    source = folder / 'source.bin'
    if not source.is_file():
        raise HTTPException(404, 'analysis not found')
    if value.reversible:
        _validate_reversible_password(value.password)
    c = conn(); row = c.execute('SELECT original_name,original,source_sha256 FROM tasks WHERE id=?', (value.analysis_id,)).fetchone(); c.close()
    if not row:
        raise HTTPException(404, 'analysis not found')
    server_filename = str(row[0] or row[1])
    if extension(value.filename) != extension(server_filename):
        raise HTTPException(422, '文件扩展名与分析任务不一致')
    data = source.read_bytes()
    expected_hash = str(row[2] or '')
    if expected_hash and _sha256(data) != expected_hash:
        raise HTTPException(409, '分析源文件已被修改')
    manifest = _analysis_payload(value.analysis_id)
    server_text = str(manifest.get('text', ''))
    # Text may be normalized by clients; the signed source and reviewed entity snapshot are authoritative.
    server_entities = manifest.get('entities', [])
    if not isinstance(server_entities, list):
        raise HTTPException(409, '分析实体格式无效')
    allow_new = extension(server_filename) in IMAGE_EXTENSIONS or extension(server_filename) == 'pdf'
    submitted_entities = server_entities if value.entities is None else value.entities
    submitted_boxes = manifest.get('boxes', []) if value.boxes is None else value.boxes
    entities = _review_entities(server_entities, submitted_entities, allow_new=allow_new)
    boxes = _validate_boxes(submitted_boxes)
    entities, boxes = _synchronize_review_selection(entities, boxes)
    file_ext = extension(server_filename)
    if file_ext == 'pdf':
        _require_complete_pdf_review(manifest, boxes)
    request_policies = _validate_policy_map(value.policies)
    if file_ext in IMAGE_EXTENSIONS and not value.image_override:
        request_policies = {}
    try:
        masked = mask_file(data, server_filename, server_text, entities, request_policies or policy_store, boxes)
    except ValueError as exc:
        raise HTTPException(422, str(exc)) from exc
    except (OSError, RuntimeError, zipfile.BadZipFile) as exc:
        raise HTTPException(422, f'文件脱敏失败: {exc}') from exc
    ext = file_ext
    output_name = f'{Path(server_filename).stem}_masked.{ext}'
    (folder / output_name).write_bytes(masked)
    report = _safe_report(server_filename, server_text, entities,
                          request_policies or policy_store,
                          reversible=value.reversible,
                          source_sha256=expected_hash,
                          boxes=boxes)
    (folder / 'report.json').write_text(json.dumps(report, ensure_ascii=False), encoding='utf-8')
    if value.reversible:
        try:
            (folder / 'mapping.enc').write_bytes(encrypt_json({'version': 1, 'filename': server_filename, 'data': data.hex(), 'sha256': expected_hash}, value.password or ''))
        except ValueError as exc:
            raise HTTPException(422, str(exc)) from exc
    # Keep plaintext source only until a successful result has been produced.
    _remove_private_analysis(folder, remove_source=True)
    update_task(value.analysis_id, output_name, artifact_name=output_name, source_path='', reversible=value.reversible)
    response: dict[str, Any] = {
        'task_id': value.analysis_id,
        'status': 'completed',
        'artifact': f'/api/tasks/{value.analysis_id}/artifacts/{output_name}',
        'report': f'/api/tasks/{value.analysis_id}/artifacts/report.json',
        'reversible': value.reversible,
        'selected_entity_count': report['selected_entity_count'],
        'selected_box_count': report['selected_box_count'],
    }
    # Preserve the original text endpoint/document shim response contract for
    # plain text uploads while keeping binary artifacts download-only.
    if ext in {'txt', 'md'}:
        response['masked_text'] = masked.decode('utf-8', 'replace')
    return response

@app.post('/api/tasks/{task_id}/restore')
def restore_file(task_id: str, value: RestoreFileIn):
    folder = _task_folder(task_id)
    c = conn()
    row = c.execute('SELECT id FROM tasks WHERE id=?', (task_id,)).fetchone()
    c.close()
    if not row or not folder.is_dir():
        raise HTTPException(404, 'task not found')
    mapping = folder / 'mapping.enc'
    if not mapping.is_file():
        raise HTTPException(409, '该任务未启用可逆模式')
    try:
        payload = decrypt_json(mapping.read_bytes(), value.password)
    except Exception as exc:
        raise HTTPException(403, '口令错误或映射已损坏') from exc
    filename = _safe_filename(str(payload.get('filename', 'restored.bin')))
    try:
        restored = bytes.fromhex(str(payload['data']))
    except (KeyError, TypeError, ValueError) as exc:
        raise HTTPException(409, '加密映射格式无效') from exc
    if payload.get('sha256') and _sha256(restored) != payload['sha256']:
        raise HTTPException(409, '恢复文件校验失败')
    restored_name = f'{Path(filename).stem}_restored.{extension(filename)}'
    (folder / restored_name).write_bytes(restored)
    c = conn()
    c.execute('UPDATE tasks SET restored_name=?, updated=? WHERE id=?',
              (restored_name, datetime.now().timestamp(), task_id))
    c.commit(); c.close()
    return {'task_id': task_id, 'artifact': f'/api/tasks/{task_id}/artifacts/{restored_name}'}

batch_lock = threading.RLock()
batch_workers: set[str] = set()

def _load_batch_state(batch_id: str) -> dict[str, Any] | None:
    """Load a batch snapshot from SQLite when the process-local cache is cold.

    Uploads and analysis manifests live on disk, while the in-memory map only
    exists for the current worker process.  All public batch endpoints use this
    loader so a browser can continue viewing or cancelling a batch after a
    development-server reload.
    """
    if not _valid_task_id(batch_id):
        return None
    with batch_lock:
        cached = batches.get(batch_id)
    if cached is not None:
        return cached
    c = conn()
    row = c.execute('SELECT metadata_json,status,archive_name FROM batches WHERE id=?', (batch_id,)).fetchone()
    c.close()
    if not row:
        return None
    try:
        state = json.loads(row[0])
    except (TypeError, json.JSONDecodeError):
        state = {'status': 'failed', 'error': '批次状态损坏'}
    if not isinstance(state, dict):
        state = {'status': 'failed', 'error': '批次状态格式无效'}
    state['status'] = str(row[1] or state.get('status', 'failed'))
    archive_name = str(row[2] or state.get('archive_name', ''))
    if archive_name:
        state['archive_name'] = archive_name
    archive = _task_path(batch_id) / archive_name if archive_name else None
    if archive is not None and archive.is_file():
        state['archive'] = f'/api/tasks/{batch_id}/artifacts/{archive.name}'
    _hydrate_batch_review_state(state)
    # Execution secrets are intentionally memory-only.  If the interpreter
    # stopped after review was submitted, require a fresh explicit review and
    # password instead of resuming with incomplete state.
    if state.get('analysis_ready') and state.get('status') in {'queued', 'running', 'analyzing'}:
        state['status'] = 'awaiting_review'
        state['cancelled'] = False
        state.pop('reviewed_items', None)
        _save_batch(batch_id, state)
    with batch_lock:
        batches[batch_id] = state
    return state

def _start_batch_worker(batch_id: str, options: BatchMaskIn | None = None) -> bool:
    """Start at most one background worker for a batch."""
    with batch_lock:
        if batch_id in batch_workers:
            return False
        batch_workers.add(batch_id)

    def runner() -> None:
        try:
            _run_batch(batch_id, options)
        except Exception as exc:
            state = _load_batch_state(batch_id)
            if state is not None:
                state['status'] = 'failed'
                state['error'] = f'批次处理异常: {exc}'
                _cleanup_batch_private_files(batch_id, state)
                _save_batch(batch_id, state)
        finally:
            with batch_lock:
                batch_workers.discard(batch_id)

    threading.Thread(target=runner, daemon=True, name=f'desensitize-batch-{batch_id[:8]}').start()
    return True

def _json_state(state: dict[str, Any]) -> dict[str, Any]:
    """Return an independent JSON-safe copy without process-only secrets."""
    result = deepcopy(state)
    for key in {'password', 'files_bytes'}:
        result.pop(key, None)
    return result


def _persistent_batch_state(state: dict[str, Any]) -> dict[str, Any]:
    """Build a restart snapshot without extracted values or review secrets."""
    result = _json_state(state)
    # User review payloads can contain plaintext values and a reversible-mode
    # password.  A restarted process deliberately returns to review instead of
    # trying to resume an execution whose secret was held only in memory.
    result.pop('reviewed_items', None)
    for raw_item in result.get('items', []):
        if not isinstance(raw_item, dict):
            continue
        raw_item.pop('text', None)
        for collection in ('entities', 'boxes'):
            values = raw_item.get(collection)
            if not isinstance(values, list):
                continue
            for value in values:
                if isinstance(value, dict):
                    value.pop('text', None)
                    value.pop('replacement', None)
    report = result.get('report')
    if isinstance(report, dict):
        report['items'] = _sanitize_batch_items(report.get('items', []))
    return result


def _hydrate_batch_review_state(state: dict[str, Any]) -> None:
    """Restore private review values from child manifests after a restart."""
    if state.get('status') in {'completed', 'partial', 'failed', 'cancelled'}:
        return
    for item in state.get('items', []):
        if not isinstance(item, dict) or not item.get('task_id'):
            continue
        try:
            manifest = _analysis_payload(str(item['task_id']))
        except HTTPException:
            continue
        item['text'] = str(manifest.get('text', ''))
        for collection in ('entities', 'boxes'):
            source = manifest.get(collection, [])
            if not isinstance(source, list):
                continue
            prior = item.get(collection, [])
            prior_by_id = {
                str(value.get('id')): value
                for value in prior
                if isinstance(value, dict) and value.get('id')
            } if isinstance(prior, list) else {}
            restored: list[dict[str, Any]] = []
            for value in source:
                if not isinstance(value, dict):
                    continue
                restored_value = dict(value)
                previous = prior_by_id.get(str(value.get('id')))
                if isinstance(previous, dict) and isinstance(previous.get('selected'), bool):
                    restored_value['selected'] = previous['selected']
                restored.append(restored_value)
            item[collection] = restored

def _save_batch(batch_id: str, state: dict[str, Any]) -> None:
    now = datetime.now().timestamp(); state['updated'] = now
    payload = _persistent_batch_state(state)
    c=conn(); c.execute('''INSERT INTO batches(id,created,updated,status,total,completed,failed,archive_name,metadata_json,error)
        VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET updated=excluded.updated,status=excluded.status,
        total=excluded.total,completed=excluded.completed,failed=excluded.failed,archive_name=excluded.archive_name,
        metadata_json=excluded.metadata_json,error=excluded.error''',
        (batch_id, state.get('created', now), now, state.get('status','queued'), state.get('total',0),
         state.get('completed',0), state.get('failed',0), state.get('archive_name',''),
         json.dumps(payload, ensure_ascii=False), state.get('error','')))
    c.commit(); c.close()

def _public_batch(state: dict[str, Any]) -> dict[str, Any]:
    result = deepcopy(_json_state(state))
    # Internal input paths are never exposed to the browser.
    for item in result.get('files', []):
        item.pop('storage_name', None)
        if result.get('status') in {'completed', 'partial', 'failed', 'cancelled'}:
            item.pop('sha256', None)
    if result.get('status') in {'completed', 'partial', 'failed', 'cancelled'}:
        result['items'] = _sanitize_batch_items(result.get('items', []))
        result.pop('reviewed_items', None)
    return result

def _read_batch_input(batch_id: str, metadata: dict[str, Any]) -> bytes:
    path = _task_path(batch_id) / 'inputs' / str(metadata.get('storage_name', ''))
    if path.parent.name != 'inputs' or not path.is_file():
        raise FileNotFoundError('batch input not found')
    data = path.read_bytes()
    expected = str(metadata.get('sha256', ''))
    if expected and _sha256(data) != expected:
        raise ValueError('批次源文件校验失败')
    return data


def _cleanup_batch_private_files(batch_id: str, state: dict[str, Any]) -> None:
    """Remove uploaded sources and unfinished child analyses for a terminal batch."""
    try:
        batch_folder = _task_path(batch_id)
    except HTTPException:
        return
    shutil.rmtree(batch_folder / 'inputs', ignore_errors=True)
    task_ids = {
        str(item.get('task_id'))
        for item in state.get('items', [])
        if isinstance(item, dict) and item.get('task_id')
    }
    # A worker can commit a child task just before the parent snapshot update.
    # Include every DB child by batch_id so an interrupted process cannot leave
    # a source file outside the visible batch state.
    c = conn()
    task_ids.update(
        str(row[0])
        for row in c.execute('SELECT id FROM tasks WHERE batch_id=?', (batch_id,)).fetchall()
    )
    c.close()
    for task_id in task_ids:
        if not _valid_task_id(task_id):
            continue
        try:
            child = _task_path(task_id)
        except HTTPException:
            continue
        _remove_private_analysis(child, remove_source=True)
        c = conn()
        c.execute(
            'UPDATE tasks SET source_path=?,status=?,updated=? WHERE id=? AND status!=?',
            ('', 'cancelled' if state.get('cancelled') else 'failed', datetime.now().timestamp(), task_id, 'completed'),
        )
        c.commit(); c.close()

def _analyze_batch_item(batch_id: str, metadata: dict[str, Any]) -> dict[str, Any]:
    filename = str(metadata['filename']); data = _read_batch_input(batch_id, metadata)
    task_id = uuid.uuid4().hex; folder = _task_path(task_id, create=True); (folder / 'source.bin').write_bytes(data)
    registered = False
    try:
        ext = extension(filename)
        pdf_page_count = None
        boxes: list[dict[str, Any]] = []
        warnings: list[dict[str, str]] = []
        if ext == 'pdf':
            text, entities, boxes, warnings, pdf_page_count = _analyze_pdf_content(data, folder)
        else:
            text = extract_text(data, filename) if ext not in IMAGE_EXTENSIONS else ''
            entities = analyze(text) if text else []
        image_width = image_height = None
        if ext in IMAGE_EXTENSIONS:
            image_width, image_height, boxes, ocr_entities, warnings = _analyze_image_content(data, folder)
            entities.extend(ocr_entities)
        manifest = content_manifest(filename, text, entities) | {
            'text': text, 'kind': ext, 'sha256': _sha256(data),
            'warnings': warnings, 'boxes': boxes,
        }
        if pdf_page_count is not None:
            manifest['page_count'] = pdf_page_count
        (folder / 'analysis.json').write_text(json.dumps(manifest, ensure_ascii=False), encoding='utf-8')
        _register_file_task(task_id, filename, ext, source_sha256=manifest['sha256'], status='awaiting_review', batch_id=batch_id)
        registered = True
        result = {'filename': filename, 'task_id': task_id, 'status': 'awaiting_review',
                  'text': text, 'entities': entities, 'boxes': boxes,
                  'sha256': manifest['sha256']}
        if image_width is not None and image_height is not None:
            result.update({'image_width': image_width, 'image_height': image_height})
        return result
    except Exception:
        shutil.rmtree(folder, ignore_errors=True)
        if registered:
            c = conn(); c.execute('DELETE FROM tasks WHERE id=?', (task_id,)); c.commit(); c.close()
        else:
            # Defensive cleanup in case registration committed and then raised.
            c = conn(); c.execute('DELETE FROM tasks WHERE id=?', (task_id,)); c.commit(); c.close()
        raise

def _unique_archive_name(directory: Path, filename: str, used: set[str]) -> str:
    # ZIP supports UTF-8 names. Preserve Chinese customer filenames while
    # removing characters that are unsafe when extracting on Windows.
    base = re.sub(r'[<>:"/\\|?*\x00-\x1f]+', '_', Path(filename).name)
    base = base.rstrip(' .')[:180] or 'result.bin'
    stem, suffix = Path(base).stem, Path(base).suffix
    candidate = f'{stem}_masked{suffix}'
    index = 2
    while candidate in used or (directory / candidate).exists():
        candidate = f'{stem}_masked_{index}{suffix}'; index += 1
    used.add(candidate); return candidate

def _run_batch(batch_id: str, options: BatchMaskIn | None = None):
    state = _load_batch_state(batch_id)
    if not state:
        return
    # First phase always analyzes and waits for an explicit review request.
    if not state.get('analysis_ready'):
        state['status'] = 'analyzing'; state.setdefault('items', []); _save_batch(batch_id, state)
        for index, metadata in enumerate(state.get('files', [])):
            if state.get('cancelled'): break
            # An interrupted worker may already have persisted an item before
            # the process exited.  Do not analyze that upload a second time.
            if index < len(state['items']) and state['items'][index].get('task_id'):
                continue
            try:
                item = _analyze_batch_item(batch_id, metadata)
                if index < len(state['items']):
                    state['items'][index] = item
                else:
                    state['items'].append(item)
            except Exception as exc:
                item = {'filename': metadata.get('filename',''), 'status': 'failed', 'error': str(exc)}
                if index < len(state['items']):
                    state['items'][index] = item
                else:
                    state['items'].append(item)
                state['failed'] = int(state.get('failed', 0)) + 1
            _save_batch(batch_id, state)
        state['analysis_ready'] = True
        successful = any(isinstance(item, dict) and item.get('task_id') for item in state.get('items', []))
        state['status'] = 'cancelled' if state.get('cancelled') else ('awaiting_review' if successful else 'failed')
        if state['status'] in {'cancelled', 'failed'}:
            _cleanup_batch_private_files(batch_id, state)
        _save_batch(batch_id, state)
        return
    if options is None:
        state['status'] = 'awaiting_review'; _save_batch(batch_id, state); return
    state['status'] = 'running'; state['completed'] = 0; state['failed'] = 0
    archive_dir = _task_path(batch_id, create=True); archive_dir.mkdir(parents=True, exist_ok=True); used=set()
    submitted_items = options.items if options.items is not None else state.get('reviewed_items', [])
    submitted = {str(item.get('task_id')): item for item in submitted_items if isinstance(item, dict) and item.get('task_id')}
    for item in state.get('items', []):
        if state.get('cancelled'): break
        if item.get('status') == 'failed':
            state['failed'] += 1
            continue
        task_id = str(item.get('task_id')); task_folder = _task_path(task_id); source = task_folder / 'source.bin'
        if item.get('status') == 'completed' and item.get('artifact') and (archive_dir / str(item['artifact'])).is_file():
            state['completed'] += 1
            used.add(str(item['artifact']))
            continue
        current = submitted.get(task_id, {})
        item['status'] = 'running'; _save_batch(batch_id, state)
        try:
            manifest = _analysis_payload(task_id); text = str(manifest.get('text',''))
            if current.get('text', text) != text:
                raise ValueError('批次复核文本与分析结果不一致')
            item_ext = extension(item['filename'])
            entities = _review_entities(manifest.get('entities', []), current.get('entities', manifest.get('entities', [])), allow_new=item_ext in IMAGE_EXTENSIONS or item_ext == 'pdf')
            boxes = _validate_boxes(current.get('boxes', item.get('boxes', manifest.get('boxes', []))))
            entities, boxes = _synchronize_review_selection(entities, boxes)
            if item_ext == 'pdf':
                _require_complete_pdf_review(manifest, boxes)
            # Persist the effective review state so the final ZIP report and
            # status response describe the artifact that was generated.
            item['entities'] = entities
            item['boxes'] = boxes
            data = source.read_bytes(); expected = str(manifest.get('sha256',''))
            if expected and _sha256(data) != expected: raise ValueError('分析源文件已被修改')
            masked = mask_file(data, item['filename'], text, entities, options.policies or policy_store, boxes)
            output_name = f'{Path(item["filename"]).stem}_masked.{extension(item["filename"])}'
            (task_folder / output_name).write_bytes(masked)
            if options.reversible:
                if not options.password: raise ValueError('可逆模式需要口令')
                (task_folder / 'mapping.enc').write_bytes(encrypt_json({'version':1,'filename':item['filename'],'data':data.hex(),'sha256':expected}, options.password))
            source.unlink(missing_ok=True)
            archive_name = _unique_archive_name(archive_dir, item['filename'], used); (archive_dir / archive_name).write_bytes(masked)
            update_task(task_id, output_name, artifact_name=output_name, source_path='', reversible=options.reversible)
            _remove_private_analysis(task_folder, remove_source=True)
            item.update({'status':'completed','artifact':archive_name}); state['completed'] += 1
        except Exception as exc:
            item.update({'status':'failed','error':str(exc)}); state['failed'] += 1
            # A failed item is not retryable from the browser and must not
            # leave its uploaded bytes or analysis snapshot on disk.
            _remove_private_analysis(task_folder, remove_source=True)
            update_task(task_id, status='failed', source_path='', error=str(exc))
        _save_batch(batch_id, state)
    final_status = 'cancelled' if state.get('cancelled') else ('partial' if state.get('failed') else 'completed')
    for raw_item in state.get('items', []):
        if isinstance(raw_item, dict) and raw_item.get('task_id'):
            try:
                _remove_private_analysis(_task_path(str(raw_item['task_id'])), remove_source=True)
            except HTTPException:
                pass
    safe_items = _sanitize_batch_items(state.get('items', []))
    report = {'batch_id':batch_id,'status':final_status,'items':safe_items,'completed':state.get('completed',0),'failed':state.get('failed',0)}
    (archive_dir / 'report.json').write_text(json.dumps(report,ensure_ascii=False),encoding='utf-8')
    archived_outputs: list[Path] = []
    with zipfile.ZipFile(archive_dir / 'results.zip','w',zipfile.ZIP_DEFLATED) as archive:
        for path in archive_dir.iterdir():
            if path.name != 'results.zip' and path.is_file():
                archive.write(path,path.name)
                if path.name != 'report.json':
                    archived_outputs.append(path)
    for path in archived_outputs:
        path.unlink(missing_ok=True)
    # Remove the original batch uploads after all derived artifacts have been
    # written.  Keep only the ZIP/report and sanitized status in persistence.
    shutil.rmtree(archive_dir / 'inputs', ignore_errors=True)
    state['status'] = final_status; state['archive'] = f'/api/tasks/{batch_id}/artifacts/results.zip'; state['archive_name']='results.zip'; state['report']=report
    state['items'] = safe_items
    state.pop('reviewed_items', None)
    state['files'] = [{'filename': str(item.get('filename', '')), 'kind': str(item.get('kind', ''))} for item in state.get('files', [])]
    _save_batch(batch_id, state)

@app.post('/api/batches')
def create_batch(files: list[UploadFile] = File(...), options: str = Form('{}')):
    if not _TEST_MODE and not models_ready():
        return _model_gate_response()
    if len(files) > MAX_BATCH_FILES:
        raise HTTPException(413, f'每批最多 {MAX_BATCH_FILES} 个文件')
    if not files:
        raise HTTPException(422, '批次至少需要一个文件')
    try:
        payload = BatchMaskIn.model_validate_json(options)
    except ValidationError as exc:
        raise HTTPException(422, f'批次参数无效: {exc}') from exc
    # Upload always enters review, even if a client sends reviewed=true.
    if payload.reversible:
        _validate_reversible_password(payload.password)
    request_policies = _validate_policy_map(payload.policies)
    items: list[dict[str, Any]] = []; total = 0; batch_id = uuid.uuid4().hex; batch_dir = _task_path(batch_id, create=True); input_dir = batch_dir / 'inputs'; input_dir.mkdir(exist_ok=True)
    try:
        for upload in files:
            filename = _safe_filename(upload.filename or 'upload.bin'); ext = extension(filename); data = upload.file.read(MAX_UPLOAD + 1); total += len(data)
            if len(data) > MAX_UPLOAD: raise HTTPException(413, f'{upload.filename} 超过 50 MB')
            if ext not in SUPPORTED: raise HTTPException(415, f'不支持的文件格式: {upload.filename}')
            _validate_upload(data, filename, ext)
            storage_name = f'{len(items):04d}_{uuid.uuid4().hex}{Path(filename).suffix.lower()}'
            (input_dir / storage_name).write_bytes(data)
            items.append({'filename':filename,'storage_name':storage_name,'sha256':_sha256(data),'kind':ext})
        if total > MAX_BATCH_BYTES: raise HTTPException(413, f'批次超过 {MAX_BATCH_BYTES} 字节')
    except Exception:
        # A rejected upload must not leave source bytes behind in the task
        # directory, especially when validation fails halfway through a batch.
        shutil.rmtree(batch_dir, ignore_errors=True)
        raise
    now=datetime.now().timestamp(); batches[batch_id] = {'status':'queued','created':now,'updated':now,'total':len(items),'completed':0,'failed':0,'files':items,'policies':request_policies,'reversible':payload.reversible,'cancelled':False}
    _save_batch(batch_id,batches[batch_id])
    _start_batch_worker(batch_id)
    return {'batch_id': batch_id, 'status': 'queued'}

@app.get('/api/batches/{batch_id}')
def batch_status(batch_id: str):
    if not _valid_task_id(batch_id): raise HTTPException(404,'batch not found')
    state = _load_batch_state(batch_id)
    if state is None: raise HTTPException(404,'batch not found')
    # A process restart loses Python threads, but the persisted upload/input
    # state is enough to resume analysis.  A queued execution is returned to
    # review so the caller can explicitly resubmit its (non-persisted) secret.
    if state.get('status') in {'queued', 'analyzing'} and not state.get('cancelled'):
        _start_batch_worker(batch_id)
    return _public_batch(state)

@app.post('/api/batches/{batch_id}/cancel')
def cancel_batch(batch_id: str):
    state = _load_batch_state(batch_id)
    if state is None: raise HTTPException(404, 'batch not found')
    if state.get('status') in {'completed', 'partial', 'failed', 'cancelled'}:
        return {'batch_id': batch_id, 'status': state.get('status')}
    state['cancelled']=True; state['status']='cancelled'
    _cleanup_batch_private_files(batch_id, state)
    _save_batch(batch_id,state)
    return {'batch_id': batch_id, 'status': 'cancelled'}

@app.post('/api/batches/{batch_id}/mask')
def mask_batch(batch_id: str, value: BatchMaskIn):
    if not _TEST_MODE and not models_ready():
        return _model_gate_response()
    state = _load_batch_state(batch_id)
    if not state:
        raise HTTPException(404, 'batch not found')
    if state.get('status') != 'awaiting_review':
        raise HTTPException(409, '批次当前不在待复核状态')
    if value.reversible:
        _validate_reversible_password(value.password)
    request_policies = _validate_policy_map(value.policies)
    state['status'] = 'queued'
    state['reversible'] = value.reversible; state['policies'] = request_policies or policy_store; state['cancelled'] = False
    state['reviewed_items'] = value.items
    _save_batch(batch_id,state)
    _start_batch_worker(batch_id, value)
    return {'batch_id': batch_id, 'status': 'queued'}

@app.get('/api/batches/{batch_id}/download')
def download_batch(batch_id: str):
    if not _valid_task_id(batch_id): raise HTTPException(404,'batch not found')
    state = _load_batch_state(batch_id)
    archive = _task_path(batch_id) / 'results.zip'
    if not state or state.get('status') not in {'completed', 'partial', 'cancelled'} or not archive.is_file():
        raise HTTPException(404, 'batch result not ready')
    return FileResponse(archive, filename=f'{batch_id}_results.zip', media_type='application/zip')
app.mount('/static',StaticFiles(directory=ROOT/'app'/'static'),name='static')
@app.get('/')
def index(): return FileResponse(ROOT/'app'/'static'/'index.html')
