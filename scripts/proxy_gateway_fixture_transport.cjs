'use strict';
// Credential-free fixture ONLY. Loaded explicitly with node --require; never by production.
const http = require('node:http');
const https = require('node:https');
const original = http.request;
function port(name) {
  const value = process.env[name];
  if (!/^[0-9]{1,5}$/.test(value || '') || Number(value) < 1 || Number(value) > 65535) {
    throw new Error('invalid fixture loopback port');
  }
  return Number(value);
}
http.request = (options, callback) => {
  if (options.hostname !== 'carry-context-proxy' || options.port !== 8787) {
    throw new Error('fixture denied unexpected HTTP destination');
  }
  return original({...options, hostname:'127.0.0.1', port:port('FIXTURE_CARRY_PORT')}, callback);
};
https.request = (options, callback) => {
  if (options.hostname !== 'api.openai.com' || options.port !== 443) {
    throw new Error('fixture denied unexpected HTTPS destination');
  }
  const path = options.headers.authorization === 'Bearer fixture-shadow-provider'
    ? '/classifier' : options.path;
  return original({...options, hostname:'127.0.0.1', port:port('FIXTURE_PROVIDER_PORT'), path}, callback);
};
