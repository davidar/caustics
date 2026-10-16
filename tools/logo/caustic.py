#!/usr/bin/env python3
"""Render the project's logo: a real caustic, simulated.

What it simulates
-----------------
Parallel light (a distant sun, slightly off vertical) falls on a water
surface whose height is a sum of gentle waves. Each ray is refracted into
the water by Snell's law (vector form, index IOR) and weighted by its
Fresnel transmittance, then travels in a straight line to a flat floor at
depth DEPTH, where it is binned. The surface is periodic, so rays that land
past one edge re-enter at the opposite one and no light is lost. Where the
waves focus neighbouring rays together the floor brightens; where they fold
the map over itself the familiar bright net appears. Nothing is drawn: the
pattern is whatever the medium computes.

Near the top edge the surface carries one long, smooth ridge across the
whole width. It is a cylindrical lens. Its crest curvature is chosen from the
paraxial focal length of a single refracting surface, f = n / ((n - 1) c),
to focus at about half the depth, so the light it gathers crosses over and
spreads into a band edged by two fold caustics, and the waves riding on it
make the band waver: the bar, sitting on the edge of the screen. Beneath it
the floor is dark, because the ridge took that light.

The light is traced in three wavelength bands, red, green and blue, each
refracted with its own index, and each band lands in its own channel of the
floor. Water's dispersion is real but tiny (about 1.331, 1.333 and 1.337
across the visible range), so the spread around the green index is scaled by
DISPERSION to show at icon size. Where rays converge to a fold the bands
overlap and the core stays pale; on the two flanks of a fold they part, and
thin warm and cool fringes appear. That is chromatic aberration as the water
makes it, not a shifted channel.

Each band's irradiance is normalised to the mean, lightly smoothed,
tone-mapped (a soft shoulder, so crossings read as light rather than as
white blobs), coloured from a darkened copy of the bar's ground SLAB_RGB to
its accent HOVER_RGB (caustics/bar_tunables.slang) and clipped to a rounded
square.

Regenerating
------------
    python3 tools/logo/caustic.py               # docs/logo.png, docs/logo-64.png
    python3 tools/logo/caustic.py --out x.png --seed 7 --size 512

Needs python3 with numpy and Pillow, about 1.6 GB of memory, and some
ten seconds. Deterministic: the same arguments give the same image.

SPDX-License-Identifier: MIT
"""
import argparse
import os
import time

import numpy as np
from PIL import Image

SLAB_RGB = np.array([0.04, 0.05, 0.08])
HOVER_RGB = np.array([0.60, 0.90, 0.98])
# Water's index at red, green and blue, as offsets from the green one.
WATER_DISPERSION = np.array([-0.002, 0.0, 0.004])


def waves(rng, count, k_min, k_max, wind, spread, slope):
    """A directional wave spectrum on the unit-periodic tile.

    Wave vectors are integer multiples of 2*pi, so the surface tiles.
    Amplitudes fall as k^-2 (a gentle, swell-like spectrum) and are then
    scaled so the RMS slope is `slope`.
    """
    ks, amps, phases = [], [], []
    seen = set()
    while len(ks) < count:
        k = rng.uniform(k_min, k_max)
        ang = wind + rng.normal(0.0, spread)
        kx, ky = int(round(k * np.cos(ang))), int(round(k * np.sin(ang)))
        if (kx, ky) in seen or (-kx, -ky) in seen or (kx, ky) == (0, 0):
            continue
        seen.add((kx, ky))
        kk = np.hypot(kx, ky)
        ks.append((kx, ky))
        amps.append(kk ** -2.0)
        phases.append(rng.uniform(0, 2 * np.pi))
    ks = 2 * np.pi * np.array(ks, dtype=np.float64)
    amps = np.array(amps)
    rms = np.sqrt(0.5 * np.sum((amps * np.hypot(ks[:, 0], ks[:, 1])) ** 2))
    amps *= slope / rms
    return ks, amps, np.array(phases)


def surface_grid(n, ks, amps, phases, ridge):
    """Height and gradient on an n x n grid of points (i + 0.5) / n.

    The wave vectors are integer multiples of 2*pi, so the sum of waves is
    exactly an inverse FFT of a sparse spectrum: exact at the grid points,
    and far cheaper than summing sines per ray. The ridge is analytic.
    """
    m = np.rint(ks / (2 * np.pi)).astype(np.int64)
    # Shift by half a cell so samples sit at cell centres.
    shift = np.exp(1j * np.pi * (m[:, 0] + m[:, 1]) / n)
    c = amps * np.exp(1j * phases) * shift * (n * n)
    out = []
    for f in (np.ones(len(c)), 1j * ks[:, 0], 1j * ks[:, 1]):
        spec = np.zeros((n, n), dtype=np.complex64)
        np.add.at(spec, (m[:, 1] % n, m[:, 0] % n), (c * f).astype(np.complex64))
        out.append(np.fft.ifft2(spec).real.astype(np.float32))
        del spec
    h, hx, hy = out
    if ridge is not None:
        y0, width, height = ridge
        y = ((np.arange(n) + 0.5) / n)[:, None]
        # Periodic distance to the ridge line, so the tile still wraps.
        d = (y - y0 + 0.5) % 1.0 - 0.5
        g = height * np.exp(-0.5 * (d / width) ** 2)
        h += g.astype(np.float32)
        hy += (-g * d / width ** 2).astype(np.float32)
    return h, hx, hy


def trace(args):
    rng = np.random.default_rng(args.seed)
    ks, amps, phases = waves(rng, args.waves, args.kmin, args.kmax,
                             np.radians(args.wind), np.radians(args.spread),
                             args.slope)
    ridge = None
    if args.ridge_focus > 0:
        # Crest curvature c = height / width^2 for focal length
        # f = ridge_focus * depth, from f = n / ((n - 1) c).
        f = args.ridge_focus * args.depth
        height = args.ior * args.ridge_width ** 2 / ((args.ior - 1) * f)
        ridge = (args.ridge_y, args.ridge_width, height)

    sun = np.array([np.sin(np.radians(args.sun_tilt)), 0.0,
                    -np.cos(np.radians(args.sun_tilt))])
    iors = args.ior + args.dispersion * WATER_DISPERSION

    n = args.rays
    H, HX, HY = surface_grid(n, ks, amps, phases, ridge)
    res = args.grid
    acc = np.zeros((3, res * res), dtype=np.float64)
    rows = max(1, (1 << 22) // n)  # about 4M rays per batch
    xs = (np.arange(n) + 0.5) / n
    for r0 in range(0, n, rows):
        r1 = min(n, r0 + rows)
        x = np.broadcast_to(xs[None, :], (r1 - r0, n))
        y = np.broadcast_to(xs[r0:r1, None], (r1 - r0, n))
        h = H[r0:r1].astype(np.float64)
        hx = HX[r0:r1].astype(np.float64)
        hy = HY[r0:r1].astype(np.float64)
        # Unit normal, pointing up out of the water.
        inv = 1.0 / np.sqrt(hx * hx + hy * hy + 1.0)
        nx, ny, nz = -hx * inv, -hy * inv, inv
        cosi = -(sun[0] * nx + sun[1] * ny + sun[2] * nz)
        for band, ior in enumerate(iors):
            eta = 1.0 / ior
            # Snell's law, vector form.
            k = 1.0 - eta * eta * (1.0 - cosi * cosi)
            cost = np.sqrt(k)
            f = eta * cosi - cost
            tx = eta * sun[0] + f * nx
            ty = eta * sun[1] + f * ny
            tz = eta * sun[2] + f * nz
            # Fresnel transmittance, unpolarised.
            rs = ((cosi - ior * cost) / (cosi + ior * cost)) ** 2
            rp = ((cost - ior * cosi) / (cost + ior * cosi)) ** 2
            w = 1.0 - 0.5 * (rs + rp)
            # Straight down to the floor at z = -DEPTH.
            s = (h + args.depth) / -tz
            # Floor position in bins, splatted bilinearly over four bins so
            # the regular ray grid does not beat against the floor grid.
            fx = ((x + s * tx) % 1.0) * res - 0.5
            fy = ((y + s * ty) % 1.0) * res - 0.5
            x0 = np.floor(fx)
            y0 = np.floor(fy)
            ax = fx - x0
            ay = fy - y0
            x0 = x0.astype(np.int64) % res
            y0 = y0.astype(np.int64) % res
            x1 = (x0 + 1) % res
            y1 = (y0 + 1) % res
            for bx, by, wt in ((x0, y0, (1 - ax) * (1 - ay)),
                               (x1, y0, ax * (1 - ay)),
                               (x0, y1, (1 - ax) * ay), (x1, y1, ax * ay)):
                acc[band] += np.bincount((by * res + bx).ravel(),
                                         weights=(w * wt).ravel(),
                                         minlength=res * res)
    return acc.reshape(3, res, res)


def blur(img, sigma):
    """Periodic Gaussian blur through the FFT."""
    if sigma <= 0:
        return img
    n = img.shape[-1]
    f = np.fft.fftfreq(n)
    g = np.exp(-2 * (np.pi * sigma) ** 2 * (f[:, None] ** 2 + f[None, :] ** 2))
    return np.real(np.fft.ifft2(np.fft.fft2(img) * g))


def tone(e, args):
    """Irradiance per band, shape (3, n, n), to linear RGB (n, n, 3)."""
    e = np.moveaxis(e / e.mean(), 0, -1)
    ground = SLAB_RGB * args.ground
    # Soft shoulder: 1 - exp(-x) never reaches white; crossings stay light.
    v = 1.0 - np.exp(-args.exposure * np.maximum(e - args.floor, 0.0))
    v = v ** args.contrast
    # Colour: dark slate ground -> pale cyan, the hottest few percent
    # leaning toward white so the light reads as light. Each channel follows
    # its own band, so where the bands part the colour parts with them.
    lum = e.mean(axis=-1, keepdims=True)
    hot = np.clip((lum - args.white_from) / (4 * args.white_from), 0, 1) ** 2
    light = HOVER_RGB * (1 - 0.35 * hot) + 0.35 * hot
    return ground + v * (light - ground)


def rounded_mask(size, radius):
    """Antialiased rounded square, a superellipse-like squircle."""
    c = (np.arange(size) + 0.5) / size * 2 - 1
    x, y = np.meshgrid(np.abs(c), np.abs(c))
    r = radius
    qx = np.maximum(x - (1 - r), 0)
    qy = np.maximum(y - (1 - r), 0)
    d = np.sqrt(qx * qx + qy * qy) - r
    d = np.where((x < 1 - r) | (y < 1 - r), np.maximum(x, y) - 1, d)
    px = 2.0 / size
    return np.clip(0.5 - d / px, 0, 1)


def to_srgb(c):
    c = np.clip(c, 0, 1)
    return np.where(c <= 0.0031308, 12.92 * c, 1.055 * c ** (1 / 2.4) - 0.055)


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    a = p.add_argument
    a("--seed", type=int, default=3, help="wave spectrum seed")
    a("--waves", type=int, default=48, help="number of waves in the sum")
    a("--kmin", type=float, default=1.5, help="waves per tile")
    a("--kmax", type=float, default=6.0, help="waves per tile")
    a("--wind", type=float, default=35.0, help="mean wave direction, degrees")
    a("--spread", type=float, default=50.0,
      help="spread of wave directions, degrees")
    a("--slope", type=float, default=0.14, help="RMS surface slope")
    a("--ior", type=float, default=1.333, help="index of the green band")
    a("--dispersion", type=float, default=3.0,
      help="multiple of water's real dispersion; 0 for one band")
    a("--depth", type=float, default=1.3, help="water depth to the floor, tile widths")
    a("--sun-tilt", type=float, default=6.0, help="degrees off vertical")
    a("--ridge-y", type=float, default=0.16, help="tile heights from the top")
    a("--ridge-width", type=float, default=0.11, help="Gaussian sigma")
    a("--ridge-focus", type=float, default=0.55,
      help="focal length in depths; 0 for no ridge")
    a("--rays", type=int, default=6144, help="rays per side, per band")
    a("--grid", type=int, default=1536, help="floor bins per side")
    a("--sigma", type=float, default=1.0, help="smoothing, floor bins")
    a("--ground", type=float, default=0.35,
      help="ground as a fraction of SLAB_RGB")
    a("--exposure", type=float, default=0.4,
      help="tone map gain above the floor")
    a("--floor", type=float, default=0.7, help="irradiance that stays ground")
    a("--contrast", type=float, default=1.6,
      help="tone map power; higher keeps the glow dark")
    a("--white-from", type=float, default=3.0,
      help="irradiance where the light starts toward white")
    a("--radius", type=float, default=0.22, help="corner radius, half-widths")
    a("--size", type=int, default=512, help="output side, pixels")
    a("--out", default=None, help="512 px image (default docs/logo.png)")
    a("--small", default=None, help="64 px copy (default beside --out)")
    args = p.parse_args()

    root = os.path.dirname(os.path.dirname(os.path.dirname(
        os.path.abspath(__file__))))
    out = args.out or os.path.join(root, "docs", "logo.png")
    small = args.small or (os.path.join(root, "docs", "logo-64.png")
                           if args.out is None else
                           os.path.splitext(out)[0] + "-64.png")

    t0 = time.time()
    e = blur(trace(args), args.sigma)
    rgb = to_srgb(tone(e, args))
    big = Image.fromarray((rgb * 255 + 0.5).astype(np.uint8), "RGB")
    big = big.resize((args.size, args.size), Image.LANCZOS)
    m = rounded_mask(args.size, args.radius)
    rgba = np.dstack([np.asarray(big), (m * 255 + 0.5).astype(np.uint8)])
    img = Image.fromarray(rgba, "RGBA")
    img.save(out, optimize=True)
    img.resize((64, 64), Image.LANCZOS).save(small, optimize=True)
    print(f"{out} {small} in {time.time() - t0:.1f}s")


if __name__ == "__main__":
    main()
