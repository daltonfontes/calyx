"""A TCP proxy that delays every byte by D ms each way: a database D*2 ms of
round trip away, on a machine that has it next door.

    python delay_proxy.py LISTEN_PORT TARGET_HOST TARGET_PORT DELAY_MS

Order is kept: each chunk is sent DELAY_MS after it arrived, after the
chunks before it.
"""
import asyncio
import sys


async def pump(reader, writer, delay):
    loop = asyncio.get_running_loop()
    queue: asyncio.Queue = asyncio.Queue()

    async def send():
        while True:
            due, data = await queue.get()
            if data is None:
                break
            wait = due - loop.time()
            if wait > 0:
                await asyncio.sleep(wait)
            writer.write(data)
            await writer.drain()
        writer.close()

    sender = asyncio.create_task(send())
    try:
        while data := await reader.read(65536):
            queue.put_nowait((loop.time() + delay, data))
    finally:
        queue.put_nowait((0, None))
        await sender


async def main():
    port, host, target, delay_ms = sys.argv[1:5]
    delay = float(delay_ms) / 1000

    async def handle(r1, w1):
        r2, w2 = await asyncio.open_connection(host, int(target))
        await asyncio.gather(pump(r1, w2, delay), pump(r2, w1, delay), return_exceptions=True)

    server = await asyncio.start_server(handle, "127.0.0.1", int(port))
    print("ready", flush=True)
    async with server:
        await server.serve_forever()


asyncio.run(main())
