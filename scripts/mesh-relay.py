#!/usr/bin/python3
"""Loopback TCP relay from a macOS agent to the coordinator.

    /usr/bin/python3 mesh-relay.py 127.0.0.1:19000 192.168.1.103:9000

**Why this exists — found 2026-09-13 on mac1.** macOS Local Network privacy
blocks an unsigned binary from reaching LAN addresses until someone clicks
Allow on the Mac: the ai-mesh agent got "No route to host (os error 65)" to
the coordinator while `nc` from the same shell, seconds later, connected.
Loopback is not "local network", and Apple's own /usr/bin/python3 started
from cron *was* allowed to reach pi1:9000 (tested). So the agent talks to
127.0.0.1 and this relays the bytes.

It forwards raw bytes, so TLS still runs end to end between the agent and the
coordinator; the agent pins the coordinator's certificate fingerprint and
ignores the address it dialled, so nothing about trust changes.

Once Allow has been clicked for the agent on the Mac (System Settings >
Privacy & Security > Local Network), this can go: remove COORDINATOR_PORT
from ~/ai-mesh/agent.env and set COORDINATOR_IP back to the coordinator.

Standard library only.
"""
import asyncio
import sys


def parse(hostport):
    host, port = hostport.rsplit(":", 1)
    return host, int(port)


async def pipe(reader, writer):
    try:
        while True:
            data = await reader.read(65536)
            if not data:
                break
            writer.write(data)
            await writer.drain()
    except (ConnectionError, OSError):
        pass
    finally:
        try:
            writer.close()
        except Exception:
            pass


async def main():
    listen_host, listen_port = parse(sys.argv[1])
    target_host, target_port = parse(sys.argv[2])

    async def handle(client_reader, client_writer):
        try:
            upstream_reader, upstream_writer = await asyncio.wait_for(
                asyncio.open_connection(target_host, target_port), timeout=10
            )
        except (OSError, asyncio.TimeoutError) as e:
            print(f"relay: cannot reach {target_host}:{target_port}: {e}", flush=True)
            client_writer.close()
            return
        await asyncio.gather(
            pipe(client_reader, upstream_writer),
            pipe(upstream_reader, client_writer),
        )

    server = await asyncio.start_server(handle, listen_host, listen_port)
    print(f"relay: {listen_host}:{listen_port} -> {target_host}:{target_port}", flush=True)
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit("usage: mesh-relay.py LISTEN_HOST:PORT TARGET_HOST:PORT")
    asyncio.run(main())
