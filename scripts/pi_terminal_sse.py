"""Bounded terminal observation, not a response decoder or transport rewriter.

Validate each JSON document while retaining only root type/response.status.
SSE data fields join with LF; LF, CRLF and CR frame endings are supported.
Depth above 128 fails closed. Strings/numbers/arrays have no length bound and
are never accumulated. Only the first 256 raw characters of a string token
are retained (longer tokens cannot equal the small protocol field names).
"""
import codecs
import json


class _TerminalJSON:
    def __init__(self):
        self.stack = []
        self.started = self.done = self.bad = False
        self.kind = self.status = None
        self.mode = None
        self.token = ''
        self.overflow = False
        self.escape = 0

    def _value(self, value=None, container=None):
        role = 'other'
        if not self.stack:
            if self.started or container != '{': raise ValueError('root object required')
            self.started = True
            return 'root'
        frame = self.stack[-1]
        kind, state, owner, key, seen = frame
        if state != 'value' and not (kind == '[' and state == 'first'):
            raise ValueError('unexpected value')
        if kind == '{':
            if owner == 'root' and key in ('type','response') or owner == 'response' and key == 'status':
                if key in seen: raise ValueError('ambiguous terminal field')
                seen.add(key)
                if owner == 'root' and key == 'type': self.kind = value
                if owner == 'root' and key == 'response' and container == '{': role = 'response'
                if owner == 'response': self.status = value
        frame[1] = 'comma'
        return role

    def _emit(self, value):
        if self.stack and self.stack[-1][0] == '{' and self.stack[-1][1] in ('first','key'):
            if not isinstance(value,str): raise ValueError('object key required')
            self.stack[-1][3] = value
            self.stack[-1][1] = 'colon'
        else: self._value(value)

    def _punctuation(self, c):
        if c in '{[':
            role = self._value(container=c)
            if len(self.stack) >= 128: raise ValueError('depth bound')
            self.stack.append([c,'first',role,None,set()])
        elif c in '}]':
            if not self.stack: raise ValueError('unmatched close')
            kind,state,*_ = self.stack[-1]
            if c != ('}' if kind == '{' else ']') or state not in ('first','comma'):
                raise ValueError('invalid close')
            self.stack.pop()
            if not self.stack: self.done = True
        elif c == ':':
            if not self.stack or self.stack[-1][1] != 'colon': raise ValueError('unexpected colon')
            self.stack[-1][1] = 'value'
        elif c == ',':
            if not self.stack or self.stack[-1][1] != 'comma': raise ValueError('unexpected comma')
            self.stack[-1][1] = 'key' if self.stack[-1][0] == '{' else 'value'
        else: raise ValueError('invalid token')

    def _character(self, c):
        if self.mode == 'string':
            # Keep lexical validity even when the discarded output is arbitrarily long.
            if self.escape > 1:
                if c not in '0123456789abcdefABCDEF': raise ValueError('unicode escape')
                self.escape -= 1
                if self.escape == 1: self.escape = 0
            elif self.escape == 1:
                if c not in '"\\/bfnrtu': raise ValueError('string escape')
                self.escape = 5 if c == 'u' else 0
            elif c == '"':
                value = json.loads('"'+self.token+'"') if not self.overflow else ''
                self.mode = None
                self._emit(value)
                return
            elif c == '\\': self.escape = 1
            elif ord(c) < 32: raise ValueError('string control')
            if len(self.token) < 256: self.token += c
            else: self.overflow = True
            return
        if self.mode == 'literal':
            if c != self.token[0]: raise ValueError('literal')
            self.token = self.token[1:]
            if not self.token:
                self.mode = None; self._emit(None)
            return
        if self.mode == 'number':
            transitions = {
                'sign': [('0123456789','integer')],
                'zero': [('.','dot'),('eE','exponent')],
                'integer': [('0123456789','integer'),('.','dot'),('eE','exponent')],
                'dot': [('0123456789','fraction')],
                'fraction': [('0123456789','fraction'),('eE','exponent')],
                'exponent': [('+-','expsign'),('0123456789','expdigits')],
                'expsign': [('0123456789','expdigits')],
                'expdigits': [('0123456789','expdigits')],
            }
            for chars,state in transitions[self.token]:
                if c in chars:
                    self.token = 'zero' if self.token == 'sign' and c == '0' else state
                    return
            if self.token not in ('zero','integer','fraction','expdigits'):
                raise ValueError('unfinished number')
            self.mode = None; self._emit(None)
            # Reprocess the delimiter; non-delimiters fail the grammar below.
        if c in ' \r\n\t': return
        if self.done: raise ValueError('trailing JSON')
        if c == '"':
            self.mode = 'string'; self.token = ''; self.overflow = False; self.escape = 0
        elif c in '-0123456789':
            self.mode = 'number'
            self.token = 'sign' if c == '-' else 'zero' if c == '0' else 'integer'
        elif c in 'tfn':
            self.mode = 'literal'; self.token = {'t':'rue','f':'alse','n':'ull'}[c]
        else: self._punctuation(c)

    def feed(self, text):
        if self.bad: return
        try:
            for c in text: self._character(c)
        except ValueError: self.bad = True

    def completed(self):
        return (not self.bad and self.done and self.mode is None and not self.stack
                and self.kind == 'response.completed' and self.status == 'completed')


class TerminalSSE:
    """Observe the final framed JSON data event, optionally followed by [DONE].

    Malformed/truncated/non-completed final data never retires intent. No SSE
    event-name or nested/output string can substitute for the two JSON fields.
    Duplicate terminal selector fields fail closed. Unknown fields are validated
    and discarded. Invalid UTF-8 invalidates observation, not forwarded bytes.
    """
    def __init__(self):
        self.decoder = codecs.getincrementaldecoder('utf-8')()
        self.bad = self.complete = False
        self.cr = False
        self._reset_event()
        self._reset_line()

    def _reset_event(self):
        self.parser = _TerminalJSON()
        self.data = False
        self.sentinel = ''

    def _reset_line(self):
        self.field = ''
        self.value = False
        self.space = False
        self.nonempty = False

    def _start_data(self):
        if self.data:
            self.parser.feed('\n')
            if len(self.sentinel) < 8: self.sentinel += '\n'
        self.data = True

    def _end_line(self):
        if not self.nonempty:
            if self.data:
                if self.sentinel != '[DONE]': self.complete = self.parser.completed()
                self._reset_event()
        elif not self.value and self.field == 'data': self._start_data()
        self._reset_line()

    def feed(self, raw):
        if self.bad: return
        try: text = self.decoder.decode(raw)
        except UnicodeDecodeError:
            self.bad = True; return
        for c in text:
            if self.cr:
                self.cr = False
                if c == '\n': continue
            if c in '\r\n':
                self._end_line(); self.cr = c == '\r'; continue
            self.nonempty = True
            if not self.value:
                if c == ':':
                    self.value = True; self.space = True
                    if self.field == 'data': self._start_data()
                elif len(self.field) < 16: self.field += c
            else:
                if self.space:
                    self.space = False
                    if c == ' ': continue
                if self.field == 'data':
                    self.parser.feed(c)
                    if len(self.sentinel) < 8: self.sentinel += c

    def completed(self):
        try: self.decoder.decode(b'',final=True)
        except UnicodeDecodeError: self.bad = True
        return not self.bad and self.complete and not self.data and not self.nonempty
