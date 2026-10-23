# Measurements

All on the reMarkable 2 (dual-core i.MX7, Cortex-A7), October 2026, from an x86
laptop. The tablet held 380 items; the full index is ~1.3 MB on the wire.

## Transports

The same work each time: list everything, mount, cold-read a 16 MB PDF (and a 9 MB
EPUB). Checksums matched the tablet's files in every run.

| Transport | `ls -R` | Mount | 16 MB PDF |
|---|---|---|---|
| TCP over USB | 0.23 s | 0.24 s | 23 MiB/s |
| SSH over USB | 0.61 s | 0.59 s | 18 MiB/s |
| TCP over Wi-Fi (temporary, no auth) | 0.39 s | 0.35 s | 15 MiB/s |
| SSH over Wi-Fi LAN | 0.76 s | 0.70 s | 13 MiB/s |
| TCP over WireGuard | 3.1 s | 3.4 s | 2–3 MiB/s |
| SSH over WireGuard | 6.3 s | 6.4 s | 1 MiB/s |

With TLS (after preferring ChaCha20):

| Transport | `ls -R` | Mount | 16 MB PDF |
|---|---|---|---|
| TCP over USB | 0.51 s (incl. cold start) | 0.30 s | 23 MiB/s |
| TLS over USB | 0.26 s | 0.30 s | 18 MiB/s (10 MiB/s with AES-GCM) |
| TLS over Wi-Fi | 0.40 s | 0.36 s | 10 MiB/s (radio-bound) |
| TCP over WireGuard | 3.1 s | 3.3 s | 2 MiB/s |

With the OpenSSL backend (same tablet):

| Transport | Mount | 16 MB PDF |
|---|---|---|
| TCP over USB | ~0.3 s | 23 MiB/s |
| TLS over USB (OpenSSL) | ~0.3 s | 23 MiB/s (rustls: 19 MiB/s that run) |
| TLS over Wi-Fi (OpenSSL) | 0.35 s | 12–17 MiB/s (radio-bound, varies run to run) |

Conclusions:

- On Wi-Fi the radio is the limit, not encryption: TLS matches unencrypted TCP.
- WireGuard is slow because traffic goes through the hub and the tablet's userspace
  wireproxy, not because of SSH.
- ssh connect alone: 0.29 s (USB), 0.41 s (LAN), 3.5 s (WireGuard).

## Startup and changes

| | Time |
|---|---|
| agent indexes 380 items on the tablet | 0.10 s |
| first connection with socket activation (systemd starts re-fsd) | 0.32 s |
| a change on the tablet appears in the mount | ~0.3 s (200 ms is debounce) |
| a rename travels as a delta | 365 bytes |
| `find` over the whole mounted tree | 7 ms |
| tree rebuild, 380 items (release) | 2.9 ms (was ~50 ms parsing full `.content`) |

## Reads and writes

| | Time |
|---|---|
| re-read of a cached 16 MB PDF | 16 ms |
| annotated PDF, 7 pages, first open | 106 ms |
| notebook render (Sample, 1 page) | 8 ms locally |
| write (mkdir, cp, mv, rm), local agent | ~10 ms (was ~285 ms debug, 55 ms before saving the index off the critical path) |
| write over ssh on the LAN | 40–100 ms |

## Sizes

| | Size |
|---|---|
| re-fsd, no TLS | 432–808 KB |
| re-fsd with rustls + ring + rcgen (static musl) | 1.79 MB |
| re-fsd with the tablet's OpenSSL (dynamic glibc) | 788 KB |
| re-fs client | 2.7 MB |
| saved index (`index.zstd`) | ~770 KB |
