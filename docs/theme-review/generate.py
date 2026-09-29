"""Review-only mockups. Reproduce current chip/palette math; no app changes."""
import colorsys
import html
import json
import math
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = Path(__file__).resolve().parent
registry = next((Path.home() / '.cargo/registry/src').glob('*/gpui-component-0.6.2'))
base = json.loads((registry / 'src/theme/default-colors.json').read_text())

def rgb(s):
    if not s.startswith('#'):
        if '-' in s:
            name, n = s.rsplit('-', 1)
            s = next(x['hex'] for x in base[name] if x['scale'] == int(n))
        else:
            s = base[s]['hex']
    return tuple(int(s[i:i+2], 16)/255 for i in (1,3,5))

def hx(c): return '#' + ''.join(f'{round(min(1,max(0,x))*255):02x}' for x in c)
def linear(x): return x/12.92 if x <= .04045 else ((x+.055)/1.055)**2.4
def lum(c): return sum(a*linear(b) for a,b in zip((.2126,.7152,.0722),c))
def ratio(a,b):
    a,b = sorted((lum(a),lum(b)))
    return (b+.05)/(a+.05)
def mix(a,b,t): return tuple(x*t+y*(1-t) for x,y in zip(a,b))
def lab(c):
    r,g,b = map(linear,c)
    l=(.4122214708*r+.5363325363*g+.0514459929*b)**(1/3)
    m=(.2119034982*r+.6806995451*g+.1073969566*b)**(1/3)
    s=(.0883024619*r+.2817188376*g+.6299787005*b)**(1/3)
    return (.2104542553*l+.793617785*m-.0040720468*s,1.9779984951*l-2.428592205*m+.4505937099*s,.0259040371*l+.7827717662*m-.808675766*s)
def from_lab(l,a,b):
    x=(l+.3963377774*a+.2158037573*b)**3
    y=(l-.1055613458*a-.0638541728*b)**3
    z=(l-.0894841775*a-1.291485548*b)**3
    cs=(4.0767416621*x-3.3077115913*y+.2309699292*z,-1.2684380046*x+2.6097574011*y-.3413193965*z,-.0041960863*x-.7034186147*y+1.707614701*z)
    return tuple(12.92*v if v<=.0031308 else 1.055*v**(1/2.4)-.055 for v in cs)
def gamut(l,a,b):
    def valid(c): return all(-.0005<=v<=1.0005 for v in c)
    result=from_lab(l,a,b)
    if not valid(result):
        lo,hi=0,1
        for _ in range(16):
            mid=(lo+hi)/2
            if valid(from_lab(l,a*mid,b*mid)): lo=mid
            else: hi=mid
        result=from_lab(l,a*lo,b*lo)
    return tuple(min(1,max(0,v)) for v in result)
def readable(c,bg,pole,target=3):
    if ratio(c,bg)>=target: return c
    l,a,b=lab(c); end=lab(pole)[0]; lo,hi=0,1
    for _ in range(16):
        mid=(lo+hi)/2
        if ratio(gamut(l+(end-l)*mid,a,b),bg)>=target: hi=mid
        else: lo=mid
    return gamut(l+(end-l)*hi,a,b)
def text(c,bg):
    if ratio(c,bg) >= 4.5: return c
    # Proposal has a little headroom above the 4.5:1 acceptance threshold.
    pole=max(((0,0,0),(1,1,1)),key=lambda p:ratio(p,bg))
    return readable(c,bg,pole,4.7)

PALETTES=json.loads((OUT/'palettes.json').read_text())
THEMES=json.loads((OUT/'before.json').read_text())
assert set(PALETTES) == set(THEMES), 'Every named variant needs its own palette'
PREVIEWS=['Default Light','Default Dark','Nord','Everforest Dark','Ayu Light','Bloomberg','Twilight']
NAMES=PREVIEWS+sorted(set(THEMES)-set(PREVIEWS))
report=[]

def resolved(t):
    c=t['colors']; bg=rgb(c['background']); fg=rgb(c['foreground']); blue=rgb(c['base.blue'])
    h,l,s=colorsys.rgb_to_hls(*blue)
    chart=[]
    for i,f in enumerate((1.4,1.2,1,.8,.6),1):
        color=rgb(c[f'chart.{i}']) if f'chart.{i}' in c else colorsys.hls_to_rgb(h,min(1,l*f),s)
        chart.append(readable(color,bg,fg))
    chips=[]
    if t['name'] in PREVIEWS:
        fill=readable(rgb(c['primary.background']),rgb(c['title_bar.background']),fg)
        pole=max((fg,bg),key=lambda p:ratio(p,fill))
        chips=[('Pinned',rgb(c['secondary.background']),rgb(c['secondary.foreground'])),('As of 14:30',mix(rgb(c.get('warning.background',c['base.yellow'])),bg,.25),fg),('Invalid scope',mix(rgb(c.get('danger.background',c['base.red'])),bg,.25),fg),('Frame pinned',fill,readable(rgb(c['primary.foreground']),fill,pole))]
    return bg,fg,chart,chips

def chart_svg(colors,fg,bg):
    grid=mix(fg,bg,.11); muted=mix(fg,bg,.62)
    s=[f'<svg viewBox="0 0 594 230" role="img" aria-label="Five synthetic overlapping time series, indexed to 100">']
    for y,label in [(26,'104'),(76,'102'),(126,'100'),(176,'98')]:
        s.append(f'<path d="M38 {y} H580" stroke="{hx(grid)}"/><text x="0" y="{y+4}" fill="{hx(muted)}">{label}</text>')
    for i,label in enumerate(['09:30','11:00','12:30','14:00','16:00']):
        s.append(f'<text x="{38+i*128}" y="220" fill="{hx(muted)}">{label}</text>')
    for i,color in enumerate(colors):
        pts=[]
        for j in range(61):
            x=38+j*9
            v=126-j*.7+18*math.sin(j*.19+i*.62)+9*math.sin(j*.57+i*.9)+i*2
            pts.append(f'{x:.1f},{v:.1f}')
        s.append(f'<polyline points="{" ".join(pts)}" fill="none" stroke="{hx(color)}" stroke-width="2" stroke-linejoin="round"/>')
        x,y=pts[-1].split(','); s.append(f'<circle cx="{x}" cy="{y}" r="3" fill="{hx(color)}"/>')
    return ''.join(s)+'</svg>'

sections=[]
for name in NAMES:
    t=THEMES[name]; bg,fg,old,chips=resolved(t)
    new=list(map(rgb,PALETTES[name]['colors']))
    # Contrast adjustment only; each palette's hue choices are explicitly authored.
    new=[readable(c,bg,fg,3.2) for c in new]
    distances=[math.dist(lab(a),lab(b)) for i,a in enumerate(new) for b in new[i+1:]]
    fresh=[(label,fill,text(ink,fill)) for label,fill,ink in chips]
    r={'name':name,'before_chart':[hx(c) for c in old],'after_chart':[hx(c) for c in new], 'chips':[{'label':a[0],'before':round(ratio(a[2],a[1]),2),'after':round(ratio(b[2],b[1]),2)} for a,b in zip(chips,fresh)],'chart_min_contrast':round(min(ratio(c,bg) for c in new),2), 'min_oklab_distance':round(min(distances),3)}
    report.append(r)
    cards=[]
    for label,colors,paints in [('Before',old,chips),('Proposed',new,fresh)]:
        legend=''.join(f'<span><i style="background:{hx(color)}"></i><b>{i+1}</b> {asset}</span>' for i,(color,asset) in enumerate(zip(colors,['SPY','QQQ','IWM','EFA','AGG'])))
        chip_markup=''.join(f'<div class="chip-sample"><span class="chip" style="background:{hx(fill)};color:{hx(ink)}">{html.escape(title)}</span><small>{ratio(ink,fill):.1f}:1</small></div>' for title,fill,ink in paints)
        cards.append(f'''<article style="--bg:{hx(bg)};--fg:{hx(fg)};--line:{hx(mix(fg,bg,.16))};--muted:{hx(mix(fg,bg,.65))}"><div class="card-title"><strong>Relative performance</strong><span>1D · Indexed to 100</span></div><div class="legend">{legend}</div>{chart_svg(colors,fg,bg)}<div class="chip-row">{chip_markup}</div></article>''')
    note=PALETTES[name]['rationale']
    sections.append(f'<section id="{name.lower().replace(" ","-")}"><h2>{name}</h2><p class="note">{note}</p><div class="labels"><span>BEFORE · CURRENT TOKENS</span><span>AFTER · PROPOSED TOKENS</span></div><div class="pair">{"".join(cards)}</div></section>')
page='''<!doctype html><html lang="en"><meta charset="utf-8"><title>Geode — theme contrast review</title><style>
*{box-sizing:border-box}body{margin:0;padding:36px 40px 44px;background:#e9ecef;color:#20262d;font:14px -apple-system,BlinkMacSystemFont,Arial,sans-serif}main{max-width:1320px;margin:auto}header{margin-bottom:28px}header .eyebrow{font-size:11px;letter-spacing:1.6px;font-weight:700;color:#5d6570}h1{font-size:30px;letter-spacing:-.6px;margin:8px 0 10px}header p{max-width:950px;font-size:15px;color:#4c5662;line-height:1.6;margin:0}section{margin:28px 0 34px}h2{font-size:19px;margin:0 0 6px}.note{margin:0 0 15px;color:#54606c;font-size:13px;max-width:1100px;line-height:1.5}.pair,.labels{display:grid;grid-template-columns:1fr 1fr;gap:20px}.labels{font-size:10px;font-weight:700;letter-spacing:1.2px;color:#54606c;margin:0 0 9px}article{background:var(--bg);color:var(--fg);padding:20px 22px;border-radius:7px;border:1px solid #939ba54a;min-width:0}.card-title{display:flex;justify-content:space-between;align-items:center}.card-title strong{font-weight:600;font-size:13px}.card-title span{color:var(--muted);font-size:11px}.legend{display:flex;gap:21px;margin:20px 0 12px;font-size:11px}.legend span{display:flex;align-items:center;gap:5px}.legend b{font-weight:500;color:var(--muted)}i{width:13px;height:3px;display:inline-block}svg{width:100%;display:block}svg text{font:10px -apple-system,BlinkMacSystemFont,Arial,sans-serif}.chip-row{border-top:1px solid var(--line);padding-top:17px;margin-top:6px;display:flex;gap:16px}.chip-sample{display:flex;flex-direction:column;gap:8px}.chip{font-size:11px;line-height:20px;padding:0 7px;border-radius:3px;white-space:nowrap}.chip-sample small{color:var(--muted);font-size:10px;font-variant-numeric:tabular-nums}footer{border-top:1px solid #c5cbd2;padding-top:18px;color:#58616c;font-size:12px;line-height:1.7}body.single header,body.single footer,body.single section{display:none}body.single section.chosen{display:block;margin:0}body.single{padding:26px 30px}body.single main{max-width:none}@media(max-width:950px){.pair,.labels{gap:10px}body{padding:20px}.legend{gap:10px}.chip-row{gap:8px}article{padding:16px}}
</style><main><header><div class="eyebrow">GEODE / DESIGN REVIEW / PROPOSAL ONLY</div><h1>A palette for each theme.</h1><p>Each of the 44 named variants has an individually chosen palette, starting from its bundled accents and neutrals. Compare identical synthetic traces below. Use the theme selector to inspect one theme; the chips retain the previous contrast proposal.</p><label style="display:block;margin-top:16px">Theme <select id="theme-select" style="margin-left:8px;padding:6px"><option value="">All 44 variants</option>'''+''.join(f'<option value="{n.lower().replace(chr(32),chr(45))}">{n}</option>' for n in NAMES)+'''</select></label></header>'''+''.join(sections)+'''<footer>Review mockup, not an application screenshot. “Before” reproduces the original theme snapshot, the pinned parser’s chart fallback, and Geode’s current 3:1 color adjustment. “After” targets ≥4.5:1 chip text (4.7:1 adjustment headroom) and ≥3:1 chart strokes. Already-readable chips stay unchanged.<br>Series numbers and labels remain visible; these colors alone do not establish accessibility for every type of color vision. Every proposal is checked for background contrast and pairwise OKLab distance. These are screening measurements, not proof of color-vision accessibility. Runtime regression tests additionally check the palettes after GPUI theme resolution.</footer></main><script>document.getElementById('theme-select').onchange=e=>{document.querySelectorAll('section').forEach(s=>s.hidden=!!e.target.value&&s.id!==e.target.value)};const id=new URLSearchParams(location.search).get('theme');if(id){document.body.classList.add('single');document.getElementById(id)?.classList.add('chosen')}</script></html>'''
(OUT/'index.html').write_text(page)
(OUT/'measurements.json').write_text(json.dumps(report,indent=2)+'\n')
print(f'Generated {len(report)} theme comparisons')
for r in sorted(report,key=lambda r:r['min_oklab_distance'])[:12]:
    print(r['name'],r['min_oklab_distance'],r['chart_min_contrast'])
