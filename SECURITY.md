# Security Policy

## Supported versions

Only the latest release receives security fixes.

## Reporting a vulnerability

Please do not open a public issue for security problems. Instead:

- Report privately via GitHub:
  [Security → Report a vulnerability](https://github.com/uccl-project/rdmatop/security/advisories/new), or
- Email the maintainer: spiderpower02@gmail.com

Include the rdmatop version, your environment (OS/kernel, driver stack),
and reproduction steps. You should get a response within a week.

## Scope notes

rdmatop is an unprivileged, read-only monitor: it reads netlink and sysfs,
and dlopens vendor libraries (NVML, amdsmi) when present. Reports of memory
unsafety in the FFI paths, or of the TUI being crashable by device-provided
data, are very welcome.
