'use strict';

const http = require('node:http');
const https = require('node:https');

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
      upstreamResponse.pipe(response);
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

module.exports = {isAllowedRequest};
if (require.main === module) serve();
