/**
 * Generate a self-signed cert at startup.
 *
 * The cert is presented by the local HTTPS server and whitelisted via
 * `session.setCertificateVerifyProc` for any host under the local suffix
 * (every `{gcid}-{userhash}.{suffix}` the mainsite may navigate to) — so we
 * don't need a real CA, and we don't weaken trust for any real origin.
 *
 * One key per process; rotates every dev-app launch.
 */

// eslint-disable-next-line @typescript-eslint/no-require-imports -- selfsigned ships CJS only
import selfsigned = require('selfsigned');

export interface CertPair {
  certPem: string;
  keyPem: string;
}

export function generateCert(localHostSuffix: string): CertPair {
  const wildcard = `*.${localHostSuffix}`;
  const pems = selfsigned.generate(
    [{ name: 'commonName', value: wildcard }],
    {
      keySize: 2048,
      days: 30,
      algorithm: 'sha256',
      extensions: [
        { name: 'basicConstraints', cA: false },
        {
          name: 'keyUsage',
          digitalSignature: true,
          keyEncipherment: true,
        },
        { name: 'extKeyUsage', serverAuth: true },
        {
          name: 'subjectAltName',
          altNames: [
            { type: 2, value: wildcard }, // DNS — wildcard SAN
            { type: 7, ip: '127.0.0.1' }, // IPv4 — for direct localhost probes
            { type: 7, ip: '::1' }, // IPv6
          ],
        },
      ],
    }
  );
  return { certPem: pems.cert, keyPem: pems.private };
}
