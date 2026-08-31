#!/usr/bin/env python3
"""Regenerates the light/dark counterpart renditions of the embedded
wallpaper artworks (see crates/chonk-shell/assets/wallpapers/SOURCES.md).

Each artwork's counterpart is derived from the committed original by a
hue-preserving luminance remap with per-artwork curves, saturation
shaping and a paper/ink tint -- the parameters below are the design,
tuned by looking at the results, not a generic filter. Run from the
repository root; requires numpy and Pillow. Overwrites the committed
*-light.png / ivory-orb-dark.png files in place.
"""
import numpy as np, os
from PIL import Image

SRC='crates/chonk-shell/assets/wallpapers'
OUT=SRC

def rgb_to_hsl(rgb):
    r,g,b = rgb[...,0],rgb[...,1],rgb[...,2]
    mx = np.max(rgb,axis=-1); mn = np.min(rgb,axis=-1)
    l = (mx+mn)/2
    d = mx-mn
    s = np.where(d==0, 0, d/(1-np.abs(2*l-1)+1e-9))
    h = np.zeros_like(l)
    m = (mx==r)&(d>0); h[m]=((g-b)[m]/d[m])%6
    m = (mx==g)&(d>0); h[m]=((b-r)[m]/d[m])+2
    m = (mx==b)&(d>0); h[m]=((r-g)[m]/d[m])+4
    return h*60, s, l

def hsl_to_rgb(h,s,l):
    c = (1-np.abs(2*l-1))*s
    hp = h/60
    x = c*(1-np.abs(hp%2-1))
    z = np.zeros_like(h)
    conds = [(hp<1),(hp<2),(hp<3),(hp<4),(hp<5),(hp>=5)]
    rgbs = [(c,x,z),(x,c,z),(z,c,x),(z,x,c),(x,z,c),(c,z,x)]
    r=np.zeros_like(h); g=np.zeros_like(h); b=np.zeros_like(h)
    done=np.zeros_like(h,dtype=bool)
    for cond,(rr,gg,bb) in zip(conds,rgbs):
        m = cond&~done
        r[m]=rr[m]; g[m]=gg[m]; b[m]=bb[m]
        done|=cond
    m_ = l-c/2
    return np.stack([r+m_,g+m_,b+m_],axis=-1)

def load(name):
    im=Image.open(os.path.join(SRC,name)).convert('RGB')
    return np.asarray(im).astype(np.float32)/255.0

def save(arr,name):
    arr=np.clip(arr,0,1)
    Image.fromarray((arr*255+0.5).astype(np.uint8)).save(os.path.join(OUT,name),optimize=True)
    print('wrote',name)

def remap(name, out, lcurve, sat, tint=None, tintw=None):
    a=load(name)
    h,s,l=rgb_to_hsl(a)
    l2=np.clip(lcurve(l),0,1)
    s2=np.clip(sat(s,l2),0,1)
    rgb=hsl_to_rgb(h,s2,l2)
    if tint is not None:
        t=np.array(tint,dtype=np.float32)/255.0
        w=np.clip(tintw(l2),0,1)[...,None]
        rgb = rgb*(1-w) + t[None,None,:]*w
    save(rgb,out)

# amber: cream paper, LEDs stay saturated deep amber
remap('amber-terminal.png','amber-terminal-light.png',
      lcurve=lambda l: 0.95-0.98*l,
      sat=lambda s,l: np.clip(s*1.15,0,1),
      tint=(246,236,211), tintw=lambda l2: np.clip((l2-0.62)/0.38,0,1)*0.85)

# graphite: as before, touch more contrast
remap('graphite-fold.png','graphite-fold-light.png',
      lcurve=lambda l: 0.96-0.85*l,
      sat=lambda s,l: s,
      tint=(246,246,244), tintw=lambda l2: np.clip((l2-0.7)/0.3,0,1)*0.5)

# teal: paper lighter, lines darker+still teal
remap('teal-blueprint.png','teal-blueprint-light.png',
      lcurve=lambda l: 0.96-0.92*l,
      sat=lambda s,l: s*(1.0-0.30*l),
      tint=(237,245,239), tintw=lambda l2: np.clip((l2-0.72)/0.28,0,1)*0.75)

# indigo: latte ground, keep wave ink
remap('indigo-waves.png','indigo-waves-light.png',
      lcurve=lambda l: 0.97-0.85*l,
      sat=lambda s,l: s*(1.0-0.35*l),
      tint=(239,241,245), tintw=lambda l2: np.clip((l2-0.70)/0.30,0,1)*0.7)

# ivory -> dark: neutral warm ink ground
remap('ivory-orb.png','ivory-orb-dark.png',
      lcurve=lambda l: 0.88-0.82*l,
      sat=lambda s,l: s*np.clip(0.12+0.55*l,0,1),
      tint=(16,15,15), tintw=lambda l2: np.clip((0.30-l2)/0.30,0,1)*0.6)

# lavender: keep (good)
remap('lavender-grid.png','lavender-grid-light.png',
      lcurve=lambda l: 0.50+0.52*l,
      sat=lambda s,l: s*0.85,
      tint=None)

# jade: hazier daylight
remap('jade-terrace.png','jade-terrace-light.png',
      lcurve=lambda l: 0.34+0.60*l,
      sat=lambda s,l: s*0.62,
      tint=(244,246,232), tintw=lambda l2: np.clip((l2-0.5)/0.5,0,1)*0.30)
