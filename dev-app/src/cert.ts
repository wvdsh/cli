/**
 * Generate a self-signed cert at startup.
 *
 * The cert is presented by the local HTTPS server and whitelisted via
 * `session.setCertificateVerifyProc` for the game subdomain only — so we
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

export function generateCert(commonName: string): CertPair {
  const pems = selfsigned.generate(
    [{ name: 'commonName', value: commonName }],
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
            { type: 2, value: commonName }, // DNS
            { type: 7, ip: '127.0.0.1' }, // IPv4 — for direct localhost probes
            { type: 7, ip: '::1' }, // IPv6
          ],
        },
      ],
    }
  );
  return { certPem: pems.cert, keyPem: pems.private };
}
