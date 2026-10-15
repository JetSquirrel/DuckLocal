#!/usr/bin/env python3
"""Size the guide's mascots for the app, from the generated originals.

    python scripts/make-mascots.py assets/mascot assets/mascot/app

Each pose is cropped to the duck, scaled to 280px tall (twice the size the
guide draws it), and written twice: as is, and with a soft light halo for
dark themes, where the black arrows and outlines would otherwise vanish.
Needs Pillow; normal builds do not run it.
"""
import sys, os
from PIL import Image, ImageFilter
src, out = sys.argv[1], sys.argv[2]
names=['duck-point-up-right','duck-point-left','duck-point-up','duck-point-down','duck-drag-file','duck-celebrate','duck-inspect']
H=280  # 2x of the 140px the card draws
for n in names:
    im=Image.open(os.path.join(src,n+'.png')).convert('RGBA')
    a=im.getchannel('A')
    # Drop near-invisible haze before cropping.
    a=a.point(lambda v: 0 if v<24 else v)
    im.putalpha(a)
    box=a.getbbox(); pad=24
    box=(max(0,box[0]-pad),max(0,box[1]-pad),min(im.width,box[2]+pad),min(im.height,box[3]+pad))
    im=im.crop(box)
    w=round(im.width*H/im.height); im=im.resize((w,H),Image.LANCZOS)
    im.save(os.path.join(out,n+'.png'),optimize=True)
    # Dark surfaces: a soft light halo under the strokes, so arrows and
    # outlines that are black on transparent still read.
    halo=im.getchannel('A').filter(ImageFilter.MaxFilter(7)).filter(ImageFilter.GaussianBlur(3))
    glow=Image.new('RGBA',im.size,(236,236,240,0)); glow.putalpha(halo.point(lambda v:int(v*0.85)))
    dark=Image.alpha_composite(glow,im)
    dark.save(os.path.join(out,n+'-dark.png'),optimize=True)
    print(n, im.size, os.path.getsize(os.path.join(out,n+'.png')), os.path.getsize(os.path.join(out,n+'-dark.png')))
