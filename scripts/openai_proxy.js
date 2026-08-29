'use strict';

const http = require('node:http');
const https = require('node:https');

const MAX_TELEMETRY_BYTES = 8 * 1024 * 1024;

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

function serve() {
  const server = http.createServer((request, response) => {
    const parsed = parsedLocalUrl(request.url);
    if (!isAllowedRequest(request.method, request.url) || !parsed) {
      response.writeHead(403, {'content-type': 'text/plain'});
      response.end('request target denied\n');
      return;
    }
    if (parsed.pathname === '/healthz') {
      response.writeHead(200, {'content-type': 'text/plain'});
      response.end('ok\n');
      return;
    }

    const upstream = https.request({
      hostname: 'api.openai.com',
      port: 443,
      method: request.method,
      path: parsed.pathname + parsed.search,
      headers: cleanHeaders(request.headers),
      timeout: 600000,
    }, upstreamResponse => {
      response.writeHead(
        upstreamResponse.statusCode || 502,
        cleanHeaders(upstreamResponse.headers),
      );
      const chunks = [];
      let captured = 0;
      upstreamResponse.on('data', chunk => {
        response.write(chunk);
        if (captured + chunk.length <= MAX_TELEMETRY_BYTES) {
          chunks.push(chunk);
          captured += chunk.length;
        }
      });
      upstreamResponse.on('end', () => {
        if (captured <= MAX_TELEMETRY_BYTES) {
          for (const usage of usageRecords(Buffer.concat(chunks).toString('utf8'))) {
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
      if (!response.headersSent) response.writeHead(502, {'content-type': 'text/plain'});
      response.end('OpenAI upstream unavailable\n');
    });
    request.on('aborted', () => upstream.destroy());
    request.pipe(upstream);
  });
  server.listen(8080, '0.0.0.0');
}

module.exports = {isAllowedRequest, usageRecords};
if (require.main === module) serve();
