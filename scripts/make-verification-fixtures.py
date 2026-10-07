#!/usr/bin/env python3
"""Create tiny, synthetic fixtures for docs/VERIFICATION.md (standard library only)."""
import argparse
import struct
import zlib
import zipfile
from pathlib import Path


def png(w, h, pixel):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    raw = b''.join(b'\0' + b''.join(bytes(pixel(x, y)) for x in range(w)) for y in range(h))
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b'')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    root = parser.parse_args().directory
    root.mkdir(parents=True, exist_ok=True)
    colors = {'red': [255, 0, 0, 255], 'white': [255] * 4, 'gray': [128, 128, 128, 255], 'green': [0, 200, 0, 255]}
    images = {name: png(64, 64, lambda x, y, c=c: c) for name, c in colors.items()}
    for name, data in images.items():
        (root / f'{name}.png').write_bytes(data)
    (root / 'transparent-edge.png').write_bytes(png(2, 1, lambda x, y: [255, 0, 0, 255] if x == 0 else [0, 0, 255, 0]))

    def layer(name, blend='svg:src-over', extra=''):
        return f'<layer name="{name}" src="data/{name}.png" opacity="1" visibility="visible" composite-op="{blend}" {extra}/>'

    def ora(name, stack, w=64, h=64):
        with zipfile.ZipFile(root / name, 'w') as archive:
            archive.writestr('mimetype', 'image/openraster')
            archive.writestr('stack.xml', f'<image version="0.0.3" w="{w}" h="{h}"><stack>{stack}</stack></image>')
            for key, data in images.items():
                archive.writestr(f'data/{key}.png', data)

    ora('merge-backdrop.ora', layer('white') + layer('gray', 'svg:multiply') + layer('red'))
    ora('merge-normal.ora', layer('white') + layer('gray') + layer('red'))
    ora('grouped.ora', '<stack visibility="hidden">' + layer('red') + '</stack>' + layer('white'))
    ora('oversized-canvas.ora', layer('white'), 1_000_000, 1_000_000)
    print(f'Created safe synthetic fixtures in {root.resolve()}')


if __name__ == '__main__':
    main()
