'use strict';

const http = require('node:http');
const https = require('node:https');

const MAX_TELEMETRY_BYTES = 8 * 1024 * 1024;
// Fixed public vocabulary, not arbitrary caller/provider strings or object keys.
const TELEMETRY_MODELS = new Set(['gpt-6-luna', 'gpt-6-sol', 'gpt-6.1-sol']);
const TELEMETRY_TIERS = new Set(['auto', 'default', 'standard', 'flex', 'priority', 'scale']);
function telemetryModel(value) {
  return TELEMETRY_MODELS.has(value) ? value : 'unrecognized';
}
function telemetryTier(value) {
  return value == null ? null : TELEMETRY_TIERS.has(value) ? value : 'unrecognized';
}
function telemetryUsage(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const result = {};
  let unknown = Object.keys(value).some(k => !['input_tokens', 'output_tokens', 'total_tokens',
    'input_tokens_details', 'output_tokens_details'].includes(k));
  const number = (source, target, key) => {
    if (Object.hasOwn(source, key)) {
      if (Number.isSafeInteger(source[key]) && source[key] >= 0) target[key] = source[key];
      else unknown = true;
    }
  };
  for (const key of ['input_tokens', 'output_tokens', 'total_tokens']) number(value, result, key);
  for (const [key, names] of [['input_tokens_details', ['cached_tokens', 'cache_write_tokens']],
    ['output_tokens_details', ['reasoning_tokens', 'audio_tokens', 'accepted_prediction_tokens', 'rejected_prediction_tokens']]]) {
    if (!Object.hasOwn(value, key)) continue;
    const details = value[key];
    result[key] = {};
    if (!details || typeof details !== 'object' || Array.isArray(details)) { unknown = true; continue; }
    if (Object.keys(details).some(k => !names.includes(k))) unknown = true;
    for (const name of names) number(details, result[key], name);
  }
  // Preserve unknown pricing semantics without retaining unknown payload or keys.
  if (unknown) {
    result.input_tokens_details ||= {};
    result.input_tokens_details.unrecognized_billing = 1;
  }
  return result;
}

const LOCAL_ORIGIN = 'http://proxy.invalid';
const HOP_BY_HOP = new Set([
  'connection', 'keep-alive', 'proxy-authenticate', 'proxy-authorization',
  'te', 'trailer', 'transfer-encoding', 'upgrade',
]);

function parsedLocalUrl(rawUrl) {
  try {
    const parsed = new URL(rawUrl, LOCAL_ORIGIN);
    return parsed.origin === LOCAL_ORIGIN ? parsed : null;
  } catch (_) {
    return null;
  }
}

function isAllowedRequest(method, rawUrl) {
  const parsed = parsedLocalUrl(rawUrl);
  if (!parsed) return false;
  if (method === 'GET' && parsed.pathname === '/healthz') return true;
  if (!['GET', 'POST', 'DELETE'].includes(method)) return false;
  return parsed.pathname === '/v1/responses' || parsed.pathname.startsWith('/v1/responses/');
}

function cleanHeaders(headers) {
  const result = {};
  for (const [name, value] of Object.entries(headers)) {
    if (!HOP_BY_HOP.has(name.toLowerCase()) && name.toLowerCase() !== 'host') {
      result[name] = value;
    }
  }
  return result;
}

function usageRecords(body) {
  const values = [];
  const visit = value => {
    if (!value || typeof value !== 'object') return;
    if (value.usage && typeof value.usage === 'object') {
      const usage = value.usage;
      if (Number.isInteger(usage.input_tokens) && usage.input_tokens >= 0) {
        values.push({
          input_tokens: usage.input_tokens,
          cached_input_tokens: Number.isInteger(usage.input_tokens_details?.cached_tokens)
            ? usage.input_tokens_details.cached_tokens : 0,
          cache_write_input_tokens: Number.isInteger(usage.input_tokens_details?.cache_write_tokens)
            ? usage.input_tokens_details.cache_write_tokens : 0,
          output_tokens: Number.isInteger(usage.output_tokens) ? usage.output_tokens : 0,
        });
      }
    }
    for (const child of Object.values(value)) visit(child);
  };
  for (const line of body.split(/\r?\n/)) {
    const payload = line.startsWith('data: ') ? line.slice(6) : line;
    if (!payload || payload === '[DONE]') continue;
    try { visit(JSON.parse(payload)); } catch (_) { /* non-JSON response fragment */ }
  }
  return values;
}

function serve(options = {}) {
  const contextProxy = process.env.BENCHMARK_CONTEXT_PROXY === '1';
  const historyPolicy = process.env.BENCHMARK_CONTEXT_HISTORY_POLICY ?? 'strict';
  if (contextProxy && !['strict', 'reset-on-divergence'].includes(historyPolicy)) {
    throw new Error('invalid trusted CARRY_PROXY_HISTORY_POLICY');
  }
  const primaryTransport = options.request || (contextProxy ? http.request : https.request);
  const shadowTransport = options.request || https.request;
  const server = http.createServer((request, response) => {
    const parsed = parsedLocalUrl(request.url);
    if (!isAllowedRequest(request.method, request.url) || !parsed) {
      response.writeHead(403, {'content-type': 'text/plain'});
      response.end('request target denied\n');
      return;
    }
    if (parsed.pathname === '/healthz') {
      if (contextProxy) {
        const health = primaryTransport({hostname: 'carry-context-proxy', port: 8787, method: 'GET',
          path: '/health', headers: {authorization: `Bearer ${process.env.CARRY_PROXY_AUTH_TOKEN}`},
          timeout: 2000}, upstreamResponse => {
          response.writeHead(upstreamResponse.statusCode === 200 ? 200 : 503);
          upstreamResponse.resume(); response.end();
        });
        health.on('timeout', () => health.destroy());
        health.on('error', () => { response.writeHead(503); response.end(); });
        health.end();
      } else {
        response.writeHead(200, {'content-type': 'text/plain'});
        response.end('ok\n');
      }
      return;
    }
    const shadow = contextProxy && Boolean(process.env.BENCHMARK_SHADOW_TOKEN) &&
      request.headers.authorization === `Bearer ${process.env.BENCHMARK_SHADOW_TOKEN}`;
    if (contextProxy && !shadow && (!process.env.BENCHMARK_CLIENT_TOKEN ||
        request.headers.authorization !== `Bearer ${process.env.BENCHMARK_CLIENT_TOKEN}`)) {
      response.writeHead(401); response.end(); return;
    }
    if (shadow && (request.method !== 'POST' || parsed.pathname !== '/v1/responses')) {
      response.writeHead(403); response.end(); return;
    }
    const headers = cleanHeaders(request.headers);
    if (contextProxy) {
      // Trusted per-slot identity. Never forward a caller-selected tenant or branch.
      for (const name of Object.keys(headers)) {
        if (name.startsWith('x-carry-')) delete headers[name];
      }
      headers.authorization = `Bearer ${shadow ? process.env.BENCHMARK_CLASSIFIER_KEY : process.env.CARRY_PROXY_AUTH_TOKEN}`;
      if (!shadow) {
        headers['x-carry-session'] = process.env.BENCHMARK_SESSION_ID;
        headers['x-carry-tenant'] = 'benchmark';
        headers['x-carry-branch'] = 'main';
        headers['x-carry-history-policy'] = historyPolicy;
      }
    }
    const throughCarry = contextProxy && !shadow;
    const actor = shadow ? 'shadow' : 'primary';
    const requestId = require('node:crypto').randomUUID();
    const started = performance.now();
    let model = null;
    let serviceTier = null;
    let nativeCompaction = parsed.pathname === '/v1/responses/compact';
    let ended = false;
    const emit = (event, extra = {}) => {
      if (contextProxy) console.log('BENCHMARK_CONTEXT_EVENT ' + JSON.stringify({
        actor, event, request_id: requestId, model, service_tier: serviceTier,
        native_compaction: nativeCompaction, ...extra,
      }));
    };
    emit('started');
    const input = [];
    let inputBytes = 0;
    request.on('data', chunk => {
      inputBytes += chunk.length;
      if (inputBytes <= MAX_TELEMETRY_BYTES) input.push(chunk);
    });
    request.on('end', () => {
      if (inputBytes > MAX_TELEMETRY_BYTES) return;
      try {
        const value = JSON.parse(Buffer.concat(input).toString('utf8'));
        model = telemetryModel(value.model);
        serviceTier = telemetryTier(value.service_tier);
        nativeCompaction ||= Boolean(value.compaction_trigger);
      } catch (_) { /* Never write request contents or parse exceptions to logs. */ }
    });
    const finish = (usage, extra = {}) => {
      if (ended) return;
      ended = true;
      emit(usage ? 'completed' : 'censored', {usage: usage || null,
        latency_ms: Math.round(performance.now() - started), ...extra});
    };
    const upstream = (shadow ? shadowTransport : primaryTransport)({
      hostname: throughCarry ? 'carry-context-proxy' : 'api.openai.com',
      port: throughCarry ? 8787 : 443,
      method: request.method,
      path: parsed.pathname + parsed.search,
      headers,
      timeout: 600000,
    }, upstreamResponse => {
      response.writeHead(
        upstreamResponse.statusCode || 502,
        cleanHeaders(upstreamResponse.headers),
      );
      const chunks = [];
      let captured = 0;
      let truncated = false;
      upstreamResponse.on('data', chunk => {
        response.write(chunk);
        if (captured + chunk.length <= MAX_TELEMETRY_BYTES) {
          chunks.push(chunk);
          captured += chunk.length;
        } else {
          truncated = true;
        }
      });
      upstreamResponse.on('error', () => { finish(null); response.destroy(); });
      upstreamResponse.on('aborted', () => { finish(null); response.destroy(); });
      upstreamResponse.on('end', () => {
        const body = Buffer.concat(chunks).toString('utf8');
        if (contextProxy) {
          let nativeUsage = null;
          let toolCalls = null;
          if (!truncated) {
            const observe = event => {
              const value = event.type === 'response.completed' ? event.response : !event.type ? event : null;
              if (value?.usage && upstreamResponse.statusCode >= 200 && upstreamResponse.statusCode < 300) {
                nativeUsage = telemetryUsage(value.usage);
                toolCalls = Array.isArray(value.output)
                  ? value.output.filter(item => item.type === 'function_call').length : null;
                if (Object.hasOwn(value, 'model')) model = telemetryModel(value.model);
                if (Object.hasOwn(value, 'service_tier')) serviceTier = telemetryTier(value.service_tier);
              }
            };
            try { observe(JSON.parse(body)); } catch (_) { /* SSE, not whole JSON. */ }
            for (const line of body.split(/\r?\n/)) {
              const payload = line.startsWith('data: ') ? line.slice(6) : line;
              try { observe(JSON.parse(payload)); }
              catch (_) { /* Non-JSON or streaming fragment, never fabricate usage. */ }
            }
          }
          finish(nativeUsage, {http_status: upstreamResponse.statusCode, tool_calls: toolCalls});
        }
        if (!contextProxy && captured <= MAX_TELEMETRY_BYTES) {
          for (const usage of usageRecords(body)) {
            // Docker logs are outside the model-controlled agent container; retain
            // only aggregate provider accounting, never prompts or responses.
            console.log(`BENCHMARK_PROXY_USAGE ${JSON.stringify(usage)}`);
          }
        }
        response.end();
      });
    });
    upstream.on('timeout', () => upstream.destroy(new Error('upstream timeout')));
    upstream.on('error', () => {
      finish(null);
      if (!response.headersSent) response.writeHead(502, {'content-type': 'text/plain'});
      response.end('OpenAI upstream unavailable\n');
    });
    request.on('aborted', () => upstream.destroy());
    request.pipe(upstream);
  });
  server.listen(options.port ?? 8080, options.host || '0.0.0.0');
  return server;
}

module.exports = {isAllowedRequest, usageRecords, serve};
if (require.main === module) serve();
