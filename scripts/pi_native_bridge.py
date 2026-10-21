"""Explicit isolated Pi RPC operator bridge; never installed or enabled implicitly.

The in-process trusted RPC controller owns native UUID/navigation selection.
HTTP callers cannot choose identity, ancestry or mapping. Raw bodies are held
only until the controller associates one observed raw cut; never journaled.
"""
import http.client
import http.server
import json
import os
from pathlib import Path
import threading
import time
from urllib.parse import urlsplit
import uuid

MAX_BODY = 16 * 1024 * 1024
MAX_MAPPING = 65536
MAX_BRANCHES = 32
HOP = {'host','connection','keep-alive','proxy-authenticate','proxy-authorization',
       'te','trailer','transfer-encoding','upgrade'}


class NativeIntentError(ValueError):
    def __init__(self, code):
        self.code = code
        super().__init__(code)


def native_path(tree):
    entries = {e['id']:e for e in tree['entries']}
    path = []; leaf = tree['leafId']
    while leaf:
        if leaf in path or leaf not in entries or len(path)>=1024:
            raise NativeIntentError('fork_incompatible')
        path.append(leaf); leaf = entries[leaf]['parentId']
    return list(reversed(path))


class PiNativeBridge:
    """One trusted operator family, bounded durable UUID+generation mapping.

    Only generation zero/new-UUID fork is supported here. Tree/rewind is NOT
    inferred from a shorter body or a filename. Unknown intent fails closed.
    """
    def __init__(self, target, token, *, tenant, session, mapping):
        url = urlsplit(target)
        if url.scheme!='http' or url.hostname!='127.0.0.1' or not url.port or url.path not in ('','/'):
            raise ValueError('explicit loopback Carry endpoint required')
        if not token or any(not x or len(x)>256 or any(ord(c)<32 for c in x) for x in (tenant,session)):
            raise ValueError('explicit trusted identity/token required')
        self.target = (url.hostname,url.port)
        self.token, self.tenant, self.session = token, tenant, session
        self.mapping = Path(mapping)
        self.lock = threading.RLock()
        self.active = None
        self.last_body = None
        self.data = {'schema_version':1,'tenant':tenant,'session':session,'natives':{}}
        if self.mapping.exists():
            raw = self.mapping.read_bytes()
            if len(raw)>MAX_MAPPING: raise ValueError('mapping bound')
            self.data = json.loads(raw)
            if (self.data.get('schema_version')!=1 or self.data.get('tenant')!=tenant
                    or self.data.get('session')!=session or not isinstance(self.data.get('natives'),dict)
                    or len(self.data['natives'])>MAX_BRANCHES):
                raise ValueError('mapping identity/schema/bound')
        outer = self
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def fail(self, status, code):
                raw = json.dumps({'error':{'code':code}}).encode()
                self.send_response(status); self.send_header('content-type','application/json')
                self.send_header('content-length',str(len(raw))); self.end_headers(); self.wfile.write(raw)
            def do_POST(self):
                if self.path!='/v1/responses': return self.fail(403,'route_denied')
                if self.headers.get_all('authorization')!=['Bearer '+outer.token]:
                    return self.fail(401,'gateway_authentication_required')
                try:
                    length = int(self.headers.get('content-length','-1'))
                    if not 0<=length<=MAX_BODY or self.headers.get('transfer-encoding'):
                        return self.fail(400,'invalid_body')
                    raw = self.rfile.read(length)
                    if len(raw)!=length: return self.fail(400,'invalid_body')
                    with outer.lock:
                        row = outer.row()
                        intent = row.get('intent')
                        if intent and intent['expires_at']<=int(time.time()):
                            raise NativeIntentError('fork_expired')
                        # Remove ALL untrusted Carry headers BEFORE trusted projection.
                        headers = {k:v for k,v in self.headers.items()
                            if k.lower() not in HOP and not k.lower().startswith('x-carry-')
                            and k.lower()!='authorization'}
                        headers.update(outer.headers(row['branch'], intent))
                        connection = http.client.HTTPConnection(*outer.target, timeout=30)
                        try:
                            connection.request('POST',self.path,raw,headers)
                            response = connection.getresponse()
                            self.send_response(response.status)
                            for k,v in response.getheaders():
                                if k.lower() not in HOP: self.send_header(k,v)
                            self.end_headers()
                            tail = b''
                            while True:
                                chunk = response.read1(65536)
                                if not chunk: break
                                tail = (tail+chunk)[-65536:]
                                self.wfile.write(chunk); self.wfile.flush()
                            complete = False
                            for line in tail.splitlines():
                                if not line.startswith(b'data: '): continue
                                try: event = json.loads(line[6:])
                                except ValueError: continue
                                if (event.get('type')=='response.completed'
                                        and event.get('response',{}).get('status')=='completed'):
                                    complete = True
                            if response.status==200 and complete:
                                # Volatile original complete input, only for the next
                                # explicit observe_boundary() after durable settlement.
                                outer.last_body = (outer.active, raw)
                                if intent:
                                    row['intent'] = None; outer.save()
                        finally:
                            connection.close()
                except NativeIntentError as error:
                    self.fail(409,error.code)
            def do_GET(self): self.fail(403,'route_denied')
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1',0), Handler)
        self.url = 'http://127.0.0.1:'+str(self.server.server_port)

    def headers(self, branch, intent=None):
        value = {'authorization':'Bearer '+self.token, 'x-carry-tenant':self.tenant,
            'x-carry-session':self.session,'x-carry-branch':branch,'x-carry-history-policy':'strict'}
        if intent:
            value.update({'x-carry-fork-parent':intent['parent'],'x-carry-fork-point':intent['id']})
        return value

    def save(self):
        raw = json.dumps(self.data, ensure_ascii=False, separators=(',',':')).encode()
        if len(raw)>MAX_MAPPING: raise NativeIntentError('native_mapping_limit')
        self.mapping.parent.mkdir(parents=True,exist_ok=True)
        temporary = self.mapping.with_suffix('.tmp')
        fd = os.open(temporary, os.O_WRONLY|os.O_CREAT|os.O_TRUNC, 0o600)
        with os.fdopen(fd,'wb') as output:
            output.write(raw); output.flush(); os.fsync(output.fileno())
        os.replace(temporary,self.mapping)

    def row(self):
        if self.active is None: raise NativeIntentError('fork_invalid_intent')
        return self.data['natives'][self.active]

    @property
    def branch(self):
        return self.row()['branch']

    def resume(self, native_id):
        # Called ONLY from trusted get_state, never a client HTTP header.
        uuid.UUID(native_id)
        with self.lock:
            if native_id not in self.data['natives']:
                if self.data['natives']: raise NativeIntentError('fork_unknown')
                self.data['natives'][native_id] = {'generation':0,'branch':'pi-'+uuid.uuid4().hex,
                    'boundary':None,'intent':None}
                self.save()
            self.active = native_id
            self.last_body = None
            return self.branch

    def observe_boundary(self, native_id, tree):
        """Associate actual settled native path with exactly the last full wire cut.

        Only an exact whole observed request boundary is supported; server matching
        supplies authority. The terminal native assistant is after that request;
        fork-before-next-user retains it, and the server validates this suffix.
        """
        with self.lock:
            if self.active!=native_id or not self.last_body or self.last_body[0]!=native_id:
                raise NativeIntentError('fork_unknown')
            path = native_path(tree)
            if not path: raise NativeIntentError('fork_incompatible')
            body = json.loads(self.last_body[1])
            connection = http.client.HTTPConnection(*self.target, timeout=30)
            try:
                connection.request('POST','/carry/fork-points/match',self.last_body[1],
                    {**self.headers(self.branch),'content-type':'application/json'})
                response = connection.getresponse(); value = json.loads(response.read())
                if response.status!=200: raise NativeIntentError('fork_incompatible')
            finally: connection.close()
            if value.get('schema_version')!=1 or value.get('matching')!='ordered-prefix-cohort-scope-v1':
                raise NativeIntentError('fork_incompatible')
            points = [p for p in value.get('points',[]) if p.get('input_items')==len(body['input'])]
            if value.get('outcome')!='matched' or len(points)!=1:
                raise NativeIntentError('fork_unknown')
            point = points[0]
            if point['expires_at']<=int(time.time()): raise NativeIntentError('fork_expired')
            self.row()['boundary'] = {**point,'native_path':path}
            self.save(); self.last_body = None
            return point

    def fork(self, parent_id, child_id, child_tree):
        uuid.UUID(child_id)
        with self.lock:
            parent = self.data['natives'].get(parent_id)
            if parent is None or not parent.get('boundary'): raise NativeIntentError('fork_unknown')
            point = parent['boundary']
            if point['expires_at']<=int(time.time()): raise NativeIntentError('fork_expired')
            if native_path(child_tree)!=point['native_path']: raise NativeIntentError('fork_incompatible')
            if child_id in self.data['natives']: raise NativeIntentError('fork_child_conflict')
            if len(self.data['natives'])>=MAX_BRANCHES: raise NativeIntentError('native_mapping_limit')
            self.data['natives'][child_id] = {'generation':0,'branch':'pi-'+uuid.uuid4().hex,
                'boundary':None,'intent':{'parent':parent['branch'],'id':point['id'],
                    'expires_at':point['expires_at']}}
            self.save(); self.active = child_id; self.last_body = None
            return self.branch

    def __enter__(self):
        self.thread = threading.Thread(target=self.server.serve_forever,daemon=True)
        self.thread.start(); return self

    def __exit__(self, *args):
        self.server.shutdown(); self.server.server_close(); self.thread.join(3)
        self.last_body = None
