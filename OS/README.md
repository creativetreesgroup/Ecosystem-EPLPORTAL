# OS

Reserved for system-operation-level configuration: things that differ between
local Docker Compose and a real deployment target (VPS overlay, systemd units,
process supervision, OS-level hardening) — the operational concerns that live
outside the application code itself. Empty as of the initial repo reorg; the
master spec's Fase 8 (VPS overlay via Traefik + ACME) is expected to be the
first real content here.
