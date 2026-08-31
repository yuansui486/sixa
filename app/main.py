from __future__ import annotations
import hashlib, io, re, shutil, sqlite3, uuid, threading, json
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any
from fastapi import FastAPI, File, HTTPException, UploadFile
from fastapi.responses import FileResponse
from fastapi.staticfiles import StaticFiles
from PIL import Image, ImageFilter
from pydantic import BaseModel, Field
from .ner import RaanerService, MODEL_ID
from .ocr import OCRService

ROOT=Path(__file__).resolve().parent.parent; DATA=ROOT/'data'; TASKS=DATA/'tasks'; MODELS=ROOT/'models'
for p in (DATA,TASKS,MODELS): p.mkdir(exist_ok=True)
DB=DATA/'tasks.db'; MAX_UPLOAD=50*1024*1024; jobs={}; ner_service=RaanerService(MODELS/'raner'); ocr_service=OCRService(MODELS/'paddleocr')
app=FastAPI(title='本地数据脱敏系统',version='0.2.0')

def conn():
    c=sqlite3.connect(DB,check_same_thread=False); c.execute('PRAGMA journal_mode=WAL')
    c.execute('CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY, original TEXT, masked TEXT, created REAL, kind TEXT DEFAULT "text")')
    if 'kind' not in {row[1] for row in c.execute('PRAGMA table_info(tasks)').fetchall()}:
        c.execute('ALTER TABLE tasks ADD COLUMN kind TEXT DEFAULT "text"')
    c.execute('CREATE TABLE IF NOT EXISTS rules(id TEXT PRIMARY KEY,name TEXT,kind TEXT,pattern TEXT,entity_type TEXT,replacement TEXT,enabled INTEGER DEFAULT 1)'); c.commit(); return c
def cleanup():
    c=conn(); cutoff=(datetime.now()-timedelta(hours=24)).timestamp(); rows=c.execute('SELECT id FROM tasks WHERE created<?',(cutoff,)).fetchall()
    for (i,) in rows: shutil.rmtree(TASKS/i,ignore_errors=True)
    c.execute('DELETE FROM tasks WHERE created<?',(cutoff,)); c.commit(); c.close()
def valid_id(v):
    if not re.fullmatch(r'\d{17}[\dXx]',v): return False
    try: datetime.strptime(v[6:14],'%Y%m%d')
    except ValueError: return False
    return '10X98765432'[sum(int(x)*w for x,w in zip(v[:17],[7,9,10,5,8,4,2,1,6,3,7,9,10,5,8,4,2]))%11]==v[-1].upper()
def luhn(v):
    return sum((n if i%2==0 else (n*2-9 if n>4 else n*2)) for i,n in enumerate(map(int,v[::-1])))%10==0
def valid_ip(v): return all(0<=int(x)<=255 for x in v.split('.'))
PATTERNS=[('PHONE',r'(?<!\d)(?:1[3-9]\d{9}|0\d{2,3}-?\d{7,8})(?!\d)',None),('ID_CARD',r'(?<!\w)\d{17}[\dXx](?!\w)',valid_id),('BANK_CARD',r'(?<!\d)\d{16,19}(?!\d)',luhn),('EMAIL',r'[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}',None),('IP_ADDRESS',r'(?<!\d)(?:\d{1,3}\.){3}\d{1,3}(?!\d)',valid_ip),('DATE_TIME',r'\d{4}[年\-/]\d{1,2}[月\-/]\d{1,2}日?(?:\s+\d{1,2}:\d{2}(?::\d{2})?)?',None),('MONEY',r'(?:人民币|RMB|￥|¥)\s?\d+(?:\.\d+)?|\d+(?:\.\d+)?(?:元|万元|亿元|美元|欧元)',None),('LICENSE_PLATE',r'[京津沪渝冀豫云辽黑湘皖鲁新苏浙赣鄂桂甘晋蒙陕吉闽贵粤青藏川宁琼][A-Z][A-Z0-9挂学警港澳领使]{5,6}',None)]
def analyze(text):
    c=conn(); custom=c.execute('SELECT name,kind,pattern,entity_type FROM rules WHERE enabled=1').fetchall(); c.close(); found=[]
    for typ,pat,check in PATTERNS+[(x[3],x[2],None) for x in custom if x[1]=='regex']:
        for m in re.finditer(pat,text,re.I):
            if check and not check(m.group()): continue
            found.append({'id':uuid.uuid4().hex,'type':typ,'text':m.group(),'start':m.start(),'end':m.end(),'score':.99 if check else .95,'recognizer':'builtin','source':'regex','selected':True,'box_ids':[]})
    for name,kind,pattern,typ in custom:
        if kind=='word':
            for m in re.finditer(re.escape(pattern),text,re.I): found.append({'id':uuid.uuid4().hex,'type':typ,'text':m.group(),'start':m.start(),'end':m.end(),'score':1.0,'recognizer':'custom','source':'custom','selected':True,'box_ids':[]})
    found.extend([{**e,'id':uuid.uuid4().hex,'selected':True,'box_ids':[]} for e in ner_service.analyze(text)])
    found.sort(key=lambda x:(x['start'],-x['end'])); accepted=[]
    for e in found:
        if not any(e['start']<a['end'] and a['start']<e['end'] for a in accepted): accepted.append(e)
    return accepted
def store(original,masked,kind='text'):
    i=uuid.uuid4().hex; d=TASKS/i; d.mkdir(); (d/'original.txt').write_text(original,encoding='utf-8'); (d/'masked.txt').write_text(masked,encoding='utf-8')
    c=conn(); c.execute('INSERT INTO tasks VALUES(?,?,?,?,?)',(i,original,masked,datetime.now().timestamp(),kind)); c.commit(); c.close(); return i

class TextIn(BaseModel): text:str=Field(min_length=1,max_length=2_000_000)
class MaskIn(BaseModel): text:str; entities:list[dict[str,Any]]
class RestoreIn(BaseModel): task_id:str; masked_text:str|None=None
class RuleIn(BaseModel): name:str; kind:str; pattern:str; entity_type:str='CUSTOM'; replacement:str='__MASKED_CUSTOM__'
class Box(BaseModel): id:str=Field(default_factory=lambda:uuid.uuid4().hex); page:int=1; x:float; y:float; width:float; height:float; text:str=''; selected:bool=True; source:str='manual'
class ImageMaskIn(BaseModel): analysis_id:str; boxes:list[Box]=[]; blur_radius:int=Field(default=14,ge=1,le=80)

@app.middleware('http')
async def housekeeping(request,call_next): cleanup(); return await call_next(request)
@app.get('/api/health')
def health(): return {'status':'ok','local_only':True,'version':app.version}
@app.get('/api/models/status')
def model_status():
    try:
        import paddleocr  # noqa: F401
        ocr = True
    except Exception:
        ocr = False
    return {'initialized':ner_service.available,'ocr_available':ocr_service.engine is not None or ocr,'device':'cpu','model':MODEL_ID,'ner_error':ner_service.error,'ocr_error':ocr_service.error}
@app.post('/api/models/initialize')
def initialize():
    i=uuid.uuid4().hex; jobs[i]={'status':'running','progress':5,'message':'正在初始化中文模型'}
    def run():
        ner_service.load(download=True, device='cpu'); ocr_service.load()
        jobs[i]={'status':'completed' if ner_service.available or ocr_service.engine else 'error','progress':100 if ner_service.available or ocr_service.engine else 0,'message':'模型初始化完成' if ner_service.available or ocr_service.engine else (ner_service.error or ocr_service.error)}
    threading.Thread(target=run,daemon=True).start(); return {'job_id':i}
@app.get('/api/models/jobs/{job_id}')
def model_job(job_id): return jobs.get(job_id,{'status':'not_found'})
@app.post('/api/text/analyze')
def text_analyze(v:TextIn): return {'analysis_id':uuid.uuid4().hex,'text':v.text,'entities':analyze(v.text)}
@app.post('/api/text/mask')
def text_mask(v:MaskIn):
    masked=v.text; seed=uuid.uuid4().hex
    for e in sorted((e for e in v.entities if e.get('selected',True)),key=lambda x:x['start'],reverse=True):
        if v.text[e['start']:e['end']]!=e['text']: raise HTTPException(422,'实体区间与原文不一致')
        token=f"__MASKED_{e['type']}_{hashlib.sha1((seed+e['text']+e['type']).encode()).hexdigest()[:8].upper()}__"; masked=masked[:e['start']]+token+masked[e['end']:]
    return {'task_id':store(v.text,masked),'masked_text':masked}
@app.post('/api/text/unmask')
def text_unmask(v:RestoreIn):
    c=conn(); row=c.execute('SELECT original,masked FROM tasks WHERE id=?',(v.task_id,)).fetchone(); c.close()
    if not row: raise HTTPException(404,'task not found')
    return {'task_id':v.task_id,'text':row[0] if v.masked_text is None else v.masked_text.replace(row[1],row[0])}
@app.post('/api/text/restore')
def text_restore(v:RestoreIn): return text_unmask(v)
@app.get('/api/tasks')
def list_tasks():
    c=conn(); rows=c.execute('SELECT id,created,kind FROM tasks ORDER BY created DESC').fetchall(); c.close(); return {'tasks':[{'id':r[0],'created':r[1],'kind':r[2]} for r in rows]}
@app.delete('/api/tasks/{task_id}')
def delete_task(task_id):
    c=conn(); c.execute('DELETE FROM tasks WHERE id=?',(task_id,)); c.commit(); c.close(); shutil.rmtree(TASKS/task_id,ignore_errors=True); return {'deleted':task_id}
@app.get('/api/tasks/{task_id}/artifacts/{name}')
def artifact(task_id,name):
    base=(TASKS/task_id).resolve(); p=(base/name).resolve()
    if not str(p).startswith(str(base)) or not p.is_file(): raise HTTPException(404,'artifact not found')
    return FileResponse(p)
@app.get('/api/rules')
def rules():
    c=conn(); rows=c.execute('SELECT id,name,kind,pattern,entity_type,replacement,enabled FROM rules').fetchall(); c.close(); keys=('id','name','kind','pattern','entity_type','replacement','enabled'); return {'rules':[dict(zip(keys,r)) for r in rows]}
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
async def image_analyze(file:UploadFile=File(...)):
    data=await file.read(MAX_UPLOAD+1)
    if len(data)>MAX_UPLOAD: raise HTTPException(413,'文件超过 50 MB')
    try: image=Image.open(io.BytesIO(data)).convert('RGB')
    except Exception as e: raise HTTPException(415,f'无法读取图片: {e}')
    i=uuid.uuid4().hex; (TASKS/i).mkdir(); image.save(TASKS/i/'original.png'); boxes=[]
    ocr_error = None
    if ocr_service.engine:
        try:
            for b in ocr_service.analyze(str(TASKS/i/'original.png')):
                boxes.append({'id':uuid.uuid4().hex,'page':1,'x':b['x1']/image.width,'y':b['y1']/image.height,'width':(b['x2']-b['x1'])/image.width,'height':(b['y2']-b['y1'])/image.height,'text':b['text'],'score':b['score'],'source':'ocr','selected':True,'entity_ids':[]})
        except RuntimeError as exc:
            ocr_error = str(exc)
    return {'analysis_id':i,'filename':file.filename,'image_width':image.width,'image_height':image.height,'ocr_available':ocr_service.engine is not None,'ocr_error':ocr_error,'boxes':boxes,'entities':[]}
@app.post('/api/image/mask')
def image_mask(v:ImageMaskIn):
    src=TASKS/v.analysis_id/'original.png'
    if not src.is_file(): raise HTTPException(404,'analysis not found')
    image=Image.open(src).convert('RGB')
    for b in v.boxes:
        if not b.selected: continue
        l,t=int(b.x*image.width),int(b.y*image.height); r,bt=int((b.x+b.width)*image.width),int((b.y+b.height)*image.height); image.paste(image.crop((l,t,r,bt)).filter(ImageFilter.GaussianBlur(v.blur_radius)),(l,t))
    image.save(src.parent/'masked.png'); return {'task_id':v.analysis_id,'artifact':f'/api/tasks/{v.analysis_id}/artifacts/masked.png'}
@app.post('/api/document/analyze')
async def document_analyze(file:UploadFile=File(...)):
    data=await file.read(MAX_UPLOAD+1)
    if len(data)>MAX_UPLOAD: raise HTTPException(413,'文件超过 50 MB')
    name=(file.filename or '').lower(); text=''
    if name.endswith(('.txt','.md')): text=data.decode('utf-8','replace')
    elif name.endswith('.docx'):
        from docx import Document; d=Document(io.BytesIO(data)); text='\n'.join(p.text for p in d.paragraphs)+'\n'+'\n'.join(c.text for t in d.tables for row in t.rows for c in row.cells)
    elif name.endswith('.pdf'):
        import fitz; text='\n'.join(p.get_text() for p in fitz.open(stream=data,filetype='pdf'))
    elif not name.endswith(('.png','.jpg','.jpeg','.bmp','.tif','.tiff')): raise HTTPException(415,'不支持的文档格式')
    analysis_id=uuid.uuid4().hex; folder=TASKS/analysis_id; folder.mkdir(); (folder/'source.bin').write_bytes(data)
    entities=analyze(text); (folder/'result.md').write_text(text,encoding='utf-8'); (folder/'content.json').write_text(json.dumps([{'page':1,'type':'text','text':text}],ensure_ascii=False),encoding='utf-8'); (folder/'entities.json').write_text(json.dumps(entities,ensure_ascii=False),encoding='utf-8')
    return {'analysis_id':analysis_id,'filename':file.filename,'text':text,'entities':entities,'content':[{'page':1,'type':'text','text':text}],'artifacts':[f'/api/tasks/{analysis_id}/artifacts/result.md',f'/api/tasks/{analysis_id}/artifacts/content.json',f'/api/tasks/{analysis_id}/artifacts/entities.json']}
@app.post('/api/document/mask')
def document_mask(v:MaskIn): return text_mask(v)
app.mount('/static',StaticFiles(directory=ROOT/'app'/'static'),name='static')
@app.get('/')
def index(): return FileResponse(ROOT/'app'/'static'/'index.html')
