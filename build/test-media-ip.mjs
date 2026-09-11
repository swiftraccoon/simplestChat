import { execFileSync as runCommand } from 'node:child_process';
import { isIP } from 'node:net';
import { networkInterfaces as readInterfaces } from 'node:os';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

function routeCommand(platform) {
  if (platform === 'darwin') return ['route', ['-n', 'get', 'default']];
  if (platform === 'linux') return ['ip', ['-j', '-4', 'route', 'show', 'default']];
  throw new Error(`Unsupported platform: ${platform}; expected linux or darwin`);
}

function defaultInterface(platform, output) {
  routeCommand(platform);
  if (typeof output !== 'string' || !output.trim()) throw new Error('Default-route command returned no output');
  if (platform === 'darwin') {
    const matches = [...output.matchAll(/^[ \t]*interface:[ \t]*(\S+)[ \t]*$/gm)];
    if (matches.length !== 1) throw new Error('Expected exactly one default-route interface from route');
    return matches[0][1];
  }

  let routes;
  try { routes = JSON.parse(output); }
  catch { throw new Error('Invalid JSON from default-route command'); }
  if (!Array.isArray(routes) || routes.length === 0) throw new Error('No IPv4 default route found');
  for (const route of routes) {
    if (!route || typeof route !== 'object' || Array.isArray(route)
      || !['default', '0.0.0.0/0'].includes(route.dst)
      || (route.type !== undefined && route.type !== 'unicast')) {
      throw new Error('Invalid IPv4 default-route entry');
    }
    if (typeof route.dev !== 'string' || !route.dev.trim() || /\s/.test(route.dev)) {
      throw new Error('Default route has no single interface; multipath routes are unsupported');
    }
    if (route.metric !== undefined && (!Number.isSafeInteger(route.metric) || route.metric < 0)) {
      throw new Error('Invalid default-route metric');
    }
  }
  const bestMetric = Math.min(...routes.map(route => route.metric ?? 0));
  const interfaces = new Set(routes.filter(route => (route.metric ?? 0) === bestMetric).map(route => route.dev));
  if (interfaces.size !== 1) throw new Error('Ambiguous IPv4 default route: equal metrics on different interfaces');
  return [...interfaces][0];
}

function usableAddress(entry) {
  if (!entry || entry.internal !== false || !['IPv4', 4].includes(entry.family)
    || typeof entry.address !== 'string' || isIP(entry.address) !== 4) return false;
  const [first, second] = entry.address.split('.').map(Number);
  return first !== 0 && first !== 127 && first < 224 && !(first === 169 && second === 254);
}

/** Select only an owned unicast IPv4 on the preferred route, never an unrelated interface. */
export function selectTestMediaIp(platform, routeOutput, interfaces) {
  const name = defaultInterface(platform, routeOutput);
  const addresses = interfaces && Object.hasOwn(interfaces, name) ? interfaces[name] : undefined;
  const address = Array.isArray(addresses) ? addresses.find(usableAddress)?.address : undefined;
  if (!address) throw new Error(`Default-route interface ${name} has no usable owned non-loopback IPv4 address`);
  return address;
}

/** Read routing/interface state only; no DNS, connection probes, or configuration changes. */
export function detectTestMediaIp({ platform = process.platform, networkInterfaces = readInterfaces, execFileSync = runCommand } = {}) {
  const [command, args] = routeCommand(platform);
  let output;
  try {
    output = execFileSync(command, args, {
      encoding: 'utf8', timeout: 3000, maxBuffer: 64 * 1024,
      stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, LC_ALL: 'C' },
    });
  } catch (error) {
    const reason = error?.code === 'ENOENT' ? 'command not installed' : 'command failed or timed out';
    throw new Error(`Cannot read default route with ${command}: ${reason}`);
  }
  return selectTestMediaIp(platform, output, networkInterfaces());
}

export function runCli({ args = [], detect = detectTestMediaIp, stdout = process.stdout, stderr = process.stderr } = {}) {
  try {
    if (args.length) throw new Error('Usage: node build/test-media-ip.mjs');
    stdout.write(`${detect()}\n`);
    return 0;
  } catch (error) {
    stderr.write(`Cannot select test media IP: ${error.message}\n`);
    return 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  process.exitCode = runCli({ args: process.argv.slice(2) });
}
