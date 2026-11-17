"""Genera icon.svg. Cambia PALETTE per un altro colore, poi: python3 gen_icon.py"""
import math
PALETTE=dict(
  bg_top="#1B57C9", bg_bot="#08205C", glow="#4C8DFF",
  outer=((10,44,118),(190,214,255)),
  mid=((14,56,138),(150,190,255)),
  ring=((18,70,170),(206,226,255)),
  ring2=((22,78,180),(222,236,255)),
  table=((60,120,225),(238,246,255)),
)
cx=cy=512
pt=lambda r,d:(cx+r*math.cos(math.radians(d)),cy-r*math.sin(math.radians(d)))
R,r,rt=292,158,84
O=[pt(R,90+60*i) for i in range(6)]; I=[pt(r,120+60*i) for i in range(6)]; T=[pt(rt,90+60*i) for i in range(6)]
light=(-0.55,0.83)
def shade(poly,lohi):
    lo,hi=lohi
    mx=sum(p[0] for p in poly)/len(poly)-cx; my=-(sum(p[1] for p in poly)/len(poly)-cy)
    n=math.hypot(mx,my) or 1; t=((mx/n*light[0]+my/n*light[1])+1)/2
    return "#%02x%02x%02x"%tuple(round(lo[k]+(hi[k]-lo[k])*t) for k in range(3))
P=lambda poly:" ".join(f"{x:.1f},{y:.1f}" for x,y in poly)
def poly(pts,col): return f'    <polygon points="{P(pts)}" fill="{col}" stroke="{col}" stroke-width="1.6"/>'
F=[]
for i in range(6):
    j=(i+1)%6
    a=[O[i],O[j],I[i]];            F.append(poly(a,shade(a,PALETTE["outer"])))
    b=[O[j],I[i],I[j]];            F.append(poly(b,shade(b,PALETTE["mid"])))
    c=[I[i],T[j],T[i]];            F.append(poly(c,shade(c,PALETTE["ring"])))
    d=[I[i],I[j],T[j]];            F.append(poly(d,shade(d,PALETTE["ring2"])))
F.append(poly(T,shade(T,PALETTE["table"])))
p=PALETTE
svg=f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0.5" y2="1"><stop offset="0" stop-color="{p['bg_top']}"/><stop offset="1" stop-color="{p['bg_bot']}"/></linearGradient>
    <radialGradient id="glow" cx="0.5" cy="0.45" r="0.55"><stop offset="0" stop-color="{p['glow']}" stop-opacity="0.5"/><stop offset="1" stop-color="{p['glow']}" stop-opacity="0"/></radialGradient>
    <linearGradient id="sheen" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity="0.18"/><stop offset="0.5" stop-color="#fff" stop-opacity="0"/></linearGradient>
    <filter id="sh" x="-20%" y="-20%" width="140%" height="150%"><feGaussianBlur stdDeviation="22"/></filter>
    <clipPath id="sq"><rect x="100" y="100" width="824" height="824" rx="184"/></clipPath>
  </defs>
  <g clip-path="url(#sq)">
    <rect x="100" y="100" width="824" height="824" fill="url(#bg)"/>
    <rect x="100" y="100" width="824" height="824" fill="url(#glow)"/>
    <rect x="100" y="100" width="824" height="412" fill="url(#sheen)"/>
  </g>
  <rect x="101" y="101" width="822" height="822" rx="183" fill="none" stroke="#fff" stroke-opacity="0.14" stroke-width="2"/>
  <polygon points="{P([(x,y+34) for x,y in O])}" fill="#000" opacity="0.5" filter="url(#sh)"/>
  <g stroke-linejoin="round">
{chr(10).join(F)}
  </g>
  <polygon points="{P(O)}" fill="none" stroke="#fff" stroke-opacity="0.28" stroke-width="3" stroke-linejoin="round"/>
  <polygon points="{P([O[5],O[0],I[5]])}" fill="#fff" opacity="0.22"/>
</svg>'''
open("icon.svg","w").write(svg)
