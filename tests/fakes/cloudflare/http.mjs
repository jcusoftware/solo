// Zero-dependency HTTP fake of the Cloudflare Workers Direct Upload & Deployments API
// on loopback, mirroring `tests/fakes/cloudflare/fake_api.rs`.
//
// Modelled on the Cloudflare docs as read on 2026-11-27:
// - https://developers.cloudflare.com/workers/static-assets/direct-upload/
// - https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/assets/subresources/upload/methods/create/
// - https://developers.cloudflare.com/api/resources/workers/subresources/assets/subresources/upload/methods/create/
// - https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/versions/methods/create/
// - https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/deployments/methods/create/
// - https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/deployments/methods/list/

import http from 'node:http';
import { createHash } from 'node:crypto';

const uuid = (n) => `${String(n).padStart(8, '0')}-0000-4000-8000-000000000000`;
const sha256Hex = (buf) => createHash('sha256').update(buf).digest('hex');

function parseMultipart(contentType, bodyBuf) {
  const m = /boundary=([^\s;]+)/i.exec(contentType || '');
  if (!m) return null;
  const boundary = m[1].replace(/^"|"$/g, '');
  const delim = Buffer.from(`--${boundary}`);
  const parts = [];
  let pos = bodyBuf.indexOf(delim);
  while (pos !== -1) {
    pos += delim.length;
    if (bodyBuf.slice(pos, pos + 2).equals(Buffer.from('--'))) break;
    if (bodyBuf.slice(pos, pos + 2).equals(Buffer.from('\r\n'))) pos += 2;
    const hdrEnd = bodyBuf.indexOf(Buffer.from('\r\n\r\n'), pos);
    if (hdrEnd === -1) break;
    const hdrText = bodyBuf.slice(pos, hdrEnd).toString('utf8');
    const bodyStart = hdrEnd + 4;
    const nextDelim = bodyBuf.indexOf(Buffer.from(`\r\n--${boundary}`), bodyStart);
    if (nextDelim === -1) break;
    const partBody = bodyBuf.slice(bodyStart, nextDelim);
    const nameM = /name="([^"]+)"/.exec(hdrText);
    const fnM = /filename="([^"]+)"/.exec(hdrText);
    const ctM = /content-type:\s*([^\r\n]+)/i.exec(hdrText);
    parts.push({
      name: nameM ? nameM[1] : '',
      filename: fnM ? fnM[1] : '',
      contentType: ctM ? ctM[1].trim() : '',
      body: partBody,
    });
    pos = nextDelim + 2;
  }
  return parts;
}

export function createFakeCloudflare({ accountId, scriptName, token }) {
  const state = {
    stored: new Set(),
    sessions: new Map(),
    completions: new Set(),
    versions: new Map(),
    deployments: [],
    uploaded: [],
    log: [],
    faults: new Map(),
    counter: 0,
  };

  const nextId = () => ++state.counter;
  const pushFault = (op, fault) => {
    if (!state.faults.has(op)) state.faults.set(op, []);
    state.faults.get(op).push(fault);
  };
  const popFault = (op) => {
    const q = state.faults.get(op);
    return q && q.length ? q.shift() : null;
  };

  const sendJson = (res, status, obj) => {
    res.writeHead(status, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(obj));
  };

  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    req.on('end', () => {
      const body = Buffer.concat(chunks);
      const url = new URL(req.url, 'http://127.0.0.1');
      const path = url.pathname;
      const auth = req.headers.authorization || '';

      let op = null;
      if (path === `/client/v4/accounts/${accountId}/workers/scripts/${scriptName}/assets-upload-session`) op = 'UploadSession';
      else if (path === `/client/v4/accounts/${accountId}/workers/assets/upload`) op = 'UploadAssets';
      else if (path === `/client/v4/accounts/${accountId}/workers/scripts/${scriptName}/versions`) op = 'CreateVersion';
      else if (path === `/client/v4/accounts/${accountId}/workers/scripts/${scriptName}/deployments` && req.method === 'POST') op = 'CreateDeployment';
      else if (path === `/client/v4/accounts/${accountId}/workers/scripts/${scriptName}/deployments` && req.method === 'GET') op = 'ListDeployments';

      state.log.push({ op, method: req.method, path, auth });

      if (!op) return sendJson(res, 404, { success: false, errors: [{ code: 10000, message: 'not found' }] });

      const fault = popFault(op);
      if (fault?.type === 'StatusBefore') {
        return sendJson(res, fault.status, { success: false, errors: [{ code: 10001, message: 'injected failure' }] });
      }
      if (fault?.type === 'DropBefore') {
        return req.socket.destroy();
      }

      if (op === 'UploadAssets') {
        const jwt = auth.startsWith('Bearer ') ? auth.slice(7) : '';
        const sess = state.sessions.get(jwt);
        if (!sess) return sendJson(res, 401, { success: false, errors: [{ code: 10002, message: 'bad upload jwt' }] });
        const parts = parseMultipart(req.headers['content-type'], body) || [];
        for (const p of parts) {
          if (!sess.pending.has(p.name)) return sendJson(res, 400, { success: false, errors: [{ code: 10003, message: 'unexpected hash' }] });
          const fpath = sess.hashToPath.get(p.name);
          const ext = fpath.split('/').pop().includes('.') ? fpath.split('/').pop().split('.').pop() : '';
          const got = sha256Hex(Buffer.from(`${p.body.toString('utf8')}${ext}`)).slice(0, 32);
          if (got !== p.name) return sendJson(res, 400, { success: false, errors: [{ code: 10004, message: 'hash mismatch' }] });
          sess.pending.delete(p.name);
          state.stored.add(p.name);
          state.uploaded.push(p.name);
        }
        if (sess.pending.size === 0) {
          const comp = `cf-complete-${nextId()}`;
          state.completions.add(comp);
          return sendJson(res, 201, { success: true, result: { jwt: comp } });
        }
        return sendJson(res, 200, { success: true, result: {} });
      }

      // All other ops require the account API token.
      if (auth !== `Bearer ${token}`) {
        return sendJson(res, 403, { success: false, errors: [{ code: 10000, message: 'unauthorized' }] });
      }

      if (op === 'UploadSession') {
        const parsed = JSON.parse(body.toString('utf8'));
        const manifest = parsed.manifest || {};
        const pending = new Set();
        const hashToPath = new Map();
        for (const [fpath, entry] of Object.entries(manifest)) {
          hashToPath.set(entry.hash, fpath);
          if (!state.stored.has(entry.hash)) pending.add(entry.hash);
        }
        if (pending.size === 0) {
          const comp = `cf-complete-${nextId()}`;
          state.completions.add(comp);
          return sendJson(res, 200, { success: true, result: { jwt: comp, buckets: [] } });
        }
        const jwt = `cf-upload-${nextId()}`;
        state.sessions.set(jwt, { pending, hashToPath });
        return sendJson(res, 200, { success: true, result: { jwt, buckets: [[...pending]] } });
      }

      if (op === 'CreateVersion') {
        const parts = parseMultipart(req.headers['content-type'], body) || [];
        const metaPart = parts.find((p) => p.name === 'metadata');
        if (!metaPart) return sendJson(res, 400, { success: false, errors: [{ code: 10005, message: 'missing metadata' }] });
        const meta = JSON.parse(metaPart.body.toString('utf8'));
        if (!state.completions.has(meta?.assets?.jwt)) {
          return sendJson(res, 400, { success: false, errors: [{ code: 10006, message: 'invalid completion jwt' }] });
        }
        if (!parts.some((p) => p.name === meta.main_module)) {
          return sendJson(res, 400, { success: false, errors: [{ code: 10007, message: 'missing main_module' }] });
        }
        const vid = uuid(nextId());
        state.versions.set(vid, meta);
        return sendJson(res, 200, { success: true, result: { id: vid } });
      }

      if (op === 'CreateDeployment') {
        const parsed = JSON.parse(body.toString('utf8'));
        const vid = parsed?.versions?.[0]?.version_id;
        if (!vid || !state.versions.has(vid)) {
          return sendJson(res, 400, { success: false, errors: [{ code: 10008, message: 'unknown version' }] });
        }
        const dep = { id: `dep-${nextId()}`, versions: parsed.versions };
        state.deployments.unshift(dep);
        if (fault?.type === 'DropAfter') return req.socket.destroy();
        if (fault?.type === 'StatusAfter') {
          return sendJson(res, fault.status, { success: false, errors: [{ code: 10009, message: 'ambiguous' }] });
        }
        return sendJson(res, 200, { success: true, result: dep });
      }

      if (op === 'ListDeployments') {
        return sendJson(res, 200, { success: true, result: { deployments: state.deployments } });
      }
    });
  });

  return { server, state, pushFault };
}
