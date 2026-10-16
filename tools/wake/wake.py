#!/usr/bin/env python3
"""Preview the bar's wake off the GPU: the same height field, refraction and
tone map as `caustics/bar.slang`'s `wake_field` and `wake_light`, stepped the
way `step_wake` steps it, drawn by numpy into a PNG. Beside the per-pixel
Jacobian picture the shader draws, it can also splat rays into a histogram
(the logo renderer's method) as a ground truth for the approximation.

Every knob is read out of the tree so the preview cannot drift from the
shader: the `WAKE_*` tunables and `SLAB_RGB`/`HOVER_RGB` from
`caustics/bar_tunables.slang`, and the `static const` constants of the wake
from `caustics/bar.slang`. `--set NAME=VALUE` overrides any of them for a
trial; the names are the shader's own without the `WAKE_` prefix.

    python3 tools/wake/wake.py --out ~/.cache/wake-preview
    python3 tools/wake/wake.py --set HEIGHT=1.4 --set DISPERSION=6

Writes, for each snapshot time after the motion stops, the shader's picture
(`wake-<t>.png`), the same with a second Newton step inverting the refraction
map (`-n2`), and the ray-splat ground truth (`-splat`), each 2000 x 64 px.
"""
import argparse
import os
import re
import sys

import numpy as np
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
TREE = os.path.normpath(os.path.join(HERE, '..', '..'))
TUNABLES = os.path.join(TREE, 'caustics', 'bar_tunables.slang')
SHADER = os.path.join(TREE, 'caustics', 'bar.slang')

# The shader's name for each knob, and the name the functions below use.
NAMES = {
    'WAKE_SOURCES': 'N', 'WAKE_SETTLE_S': 'LIFE', 'WAKE_SPACING': 'SPACING',
    'WAKE_SPEED': 'SPEED', 'WAKE_WAVELENGTH': 'WAVELENGTH', 'WAKE_HEIGHT': 'HEIGHT',
    'WAKE_FULL_SPEED': 'FULL_SPEED', 'WAKE_DEPTH': 'DEPTH', 'WAKE_IOR': 'IOR',
    'WAKE_DISPERSION': 'DISPERSION', 'WAKE_ALPHA': 'ALPHA', 'WAKE_CLICK': 'CLICK',
    'WAKE_EXPOSURE': 'EXPOSURE', 'WAKE_CONTRAST': 'CONTRAST', 'WAKE_IMAX': 'IMAX',
    'WAKE_TRAIN': 'ENVELOPE', 'WAKE_RISE_S': 'RISE', 'WAKE_MIN_HEIGHT': 'MIN_HEIGHT',
}


def read_knobs():
    """Every WAKE_ constant the two Slang files declare, by the short name."""
    text = open(TUNABLES).read() + open(SHADER).read()
    knobs = {}
    for name, short in NAMES.items():
        m = re.search(r'\b' + name + r'\s*=\s*([0-9.]+)u?;', text)
        if not m:
            sys.exit(f'{name} is not declared in the tree')
        knobs[short] = float(m.group(1))
    return knobs


def read_rgb(name):
    m = re.search(r'\b' + name + r'\s*=\s*float3\(([^)]*)\)', open(TUNABLES).read())
    if not m:
        sys.exit(f'{name} is not declared in the tunables')
    return np.array([float(v) for v in m.group(1).split(',')])


T = read_knobs()
SLAB = read_rgb('SLAB_RGB')
SLAB_A = 0.85  # the slab's alpha as drawn over a dark wallpaper
HOVER = read_rgb('HOVER_RGB')
# Water's dispersion, as `WAKE_IOR_SPREAD` in the shader: offsets from the green index.
WATER = np.array([-0.002, 0.0, 0.004])


class Wake:
    def __init__(s, t):
        s.t = t
        s.src = np.zeros((int(t['N']), 4))  # x y age amp ; amp 0 = dead
        s.track = np.array([-1.0, -1.0, 0.0, 0.0])  # last x, last y, travel, since drop
        s.next = 0

    def drop(s, x, y, a):
        s.src[s.next] = (x, y, 0.0, a)
        s.next = (s.next + 1) % int(s.t['N'])

    def step(s, ptr, dt, sc=1.0):
        t = s.t
        s.src[:, 2] += dt
        s.src[s.src[:, 2] >= t['LIFE'], 3] = 0.0
        lx, ly, travel, since = s.track
        since += dt
        if ptr is None:
            s.track = np.array([-1.0, -1.0, 0.0, since])
        elif lx < 0:
            s.track = np.array([ptr[0], ptr[1], 0.0, since])
        else:
            d = np.hypot(ptr[0] - lx, ptr[1] - ly)
            travel += d
            speed = d / max(dt, 1e-3) / sc
            if travel >= t['SPACING'] * sc and since >= t['LIFE'] / t['N']:
                a = min(speed / t['FULL_SPEED'], 1.0)
                if a > t['MIN_HEIGHT']:
                    s.drop(ptr[0], ptr[1], a)
                travel = 0.0
                since = 0.0
            s.track = np.array([ptr[0], ptr[1], travel, since])
        return bool((s.src[:, 3] > 0).any())


def field(src, X, Y, t, sc):
    """Gradient and Hessian of the height field at (X, Y), bar px."""
    k = 2 * np.pi / (t['WAVELENGTH'] * sc)
    sig = t['ENVELOPE'] * t['WAVELENGTH'] * sc
    gx = np.zeros_like(X); gy = np.zeros_like(X)
    hxx = np.zeros_like(X); hyy = np.zeros_like(X); hxy = np.zeros_like(X)
    for x0, y0, age, amp in src:
        if amp <= 0:
            continue
        life = age / t['LIFE']
        a = amp * t['HEIGHT'] * sc * (1 - life) ** 2 * min(age / t['RISE'], 1.0)
        R = t['SPEED'] * sc * age
        a /= np.sqrt(1.0 + R / (t['WAVELENGTH'] * sc))
        dx = X - x0; dy = Y - y0
        r = np.sqrt(dx * dx + dy * dy)
        u = r - R
        g = np.exp(-(u / sig) ** 2)
        g1 = -2 * u / sig ** 2 * g
        g2 = (4 * u * u / sig ** 4 - 2 / sig ** 2) * g
        c = np.cos(k * u); sn = np.sin(k * u)
        w1 = g1 * c - g * k * sn
        w2 = g2 * c - 2 * g1 * k * sn - g * k * k * c
        rr = np.maximum(r, 0.5 * sig)
        ux = dx / rr; uy = dy / rr
        wt = w1 / rr
        gx += a * w1 * ux; gy += a * w1 * uy
        hxx += a * (w2 * ux * ux + wt * uy * uy)
        hyy += a * (w2 * uy * uy + wt * ux * ux)
        hxy += a * (w2 - wt) * ux * uy
    return gx, gy, hxx, hyy, hxy


def render(w, Wpx, Hpx, sc, newton=1):
    t = w.t
    Y, X = np.mgrid[0:Hpx, 0:Wpx] + 0.5
    D = t['DEPTH'] * sc
    iors = t['IOR'] + t['DISPERSION'] * WATER
    cs = D * (1 - 1 / iors)
    gx, gy, *_ = field(w.src, X, Y, t, sc)
    x1 = X - cs[1] * gx; y1 = Y - cs[1] * gy
    for _ in range(newton - 1):
        gx, gy, hxx, hyy, hxy = field(w.src, x1, y1, t, sc)
        fx = x1 + cs[1] * gx - X; fy = y1 + cs[1] * gy - Y
        a = 1 + cs[1] * hxx; b = cs[1] * hxy; d = 1 + cs[1] * hyy
        det = a * d - b * b
        det = np.where(np.abs(det) < 0.05, np.sign(det + 1e-9) * 0.05, det)
        x1 = x1 - (d * fx - b * fy) / det
        y1 = y1 - (-b * fx + a * fy) / det
    _, _, hxx, hyy, hxy = field(w.src, x1, y1, t, sc)
    I = []
    for c in cs:
        det = (1 + c * hxx) * (1 + c * hyy) - (c * hxy) ** 2
        I.append(np.minimum(1 / np.maximum(np.abs(det), 1e-6), t['IMAX']))
    I = np.stack(I, -1)
    return tone(I, t)


def tone(I, t):
    v = (1 - np.exp(-t['EXPOSURE'] * np.maximum(I - 1, 0))) ** t['CONTRAST']
    m = v.max(-1, keepdims=True)
    lum = I.mean(-1, keepdims=True)
    hot = np.clip((lum - 2.5) / 3, 0, 1)
    light = HOVER * (1 - 0.5 * hot) + 0.5 * hot
    a = t['ALPHA'] * m
    rgb = light * np.where(m > 1e-6, v / np.maximum(m, 1e-6), 0)
    col = SLAB * SLAB_A
    out = rgb * a + col * (1 - a)
    return out


def splat(w, Wpx, Hpx, sc, ss=6):
    t = w.t
    n = ss
    Y, X = np.mgrid[0:Hpx * n, 0:Wpx * n]
    X = (X + 0.5) / n; Y = (Y + 0.5) / n
    D = t['DEPTH'] * sc
    iors = t['IOR'] + t['DISPERSION'] * WATER
    gx, gy, *_ = field(w.src, X, Y, t, sc)
    I = []
    for ior in iors:
        c = D * (1 - 1 / ior)
        fx = np.floor(X + c * gx).astype(int); fy = np.floor(Y + c * gy).astype(int)
        ok = (fx >= 0) & (fx < Wpx) & (fy >= 0) & (fy < Hpx)
        acc = np.bincount((fy[ok] * Wpx + fx[ok]), minlength=Wpx * Hpx).reshape(Hpx, Wpx)
        I.append(np.minimum(acc / (n * n), t['IMAX']))
    return tone(np.stack(I, -1), t)


def srgb(c):
    c = np.clip(c, 0, 1)
    return np.where(c <= 0.0031308, 12.92 * c, 1.055 * c ** (1 / 2.4) - 0.055)


def save(img, path, zoom=1):
    im = Image.fromarray((srgb(img) * 255 + 0.5).astype(np.uint8))
    if zoom != 1:
        im = im.resize((im.width * zoom, im.height * zoom), Image.NEAREST)
    im.save(path)


def sim(t, sc=2.0, speed=1100.0, run=0.55, after=(0.0, 0.35), fps=60):
    w = Wake(t)
    Wpx, Hpx = int(1000 * sc), int(32 * sc)
    dt = 1 / fps
    x = 120 * sc
    shots = {}
    frames = 0
    tt = 0.0
    alive = True
    while True:
        moving = tt < run
        if moving:
            x += speed * sc * dt
            ptr = (x, 16 * sc + 5 * sc * np.sin(tt * 9))
        else:
            ptr = (x, 16 * sc + 5 * sc * np.sin(run * 9))
        alive = w.step(ptr, dt, sc)
        tt += dt
        frames += 1
        for a in after:
            if abs(tt - (run + a)) < dt / 2 and a not in shots:
                shots[a] = w.src.copy()
        if not moving and not alive:
            break
        if tt > 5:
            break
    return w, shots, tt - run, Wpx, Hpx


if __name__ == '__main__':
    ap = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    ap.add_argument('--out', default=os.path.join(os.path.expanduser('~'), '.cache', 'wake-preview'),
                    help='directory the PNGs go to (default ~/.cache/wake-preview)')
    ap.add_argument('--set', action='append', default=[], metavar='NAME=VALUE',
                    help='override one knob by its short name (HEIGHT, DISPERSION, ...)')
    ap.add_argument('--speed', type=float, default=1100.0, help='the simulated sweep, logical px/s')
    args = ap.parse_args()
    t = dict(T)
    for kv in args.set:
        k, v = kv.split('=')
        if k not in t:
            sys.exit(f'{k} is not a knob; one of {sorted(t)}')
        t[k] = float(v)
    os.makedirs(args.out, exist_ok=True)
    w, shots, settle, Wpx, Hpx = sim(t, speed=args.speed)
    print(f'settled {settle:.2f}s after the motion stopped')
    for a, src in shots.items():
        w.src = src
        save(render(w, Wpx, Hpx, 2.0), f'{args.out}/wake-{a:.2f}.png')
        save(render(w, Wpx, Hpx, 2.0, newton=2), f'{args.out}/wake-{a:.2f}-n2.png')
        save(splat(w, Wpx, Hpx, 2.0), f'{args.out}/wake-{a:.2f}-splat.png')
    print(f'wrote {3 * len(shots)} pictures to {args.out}')
