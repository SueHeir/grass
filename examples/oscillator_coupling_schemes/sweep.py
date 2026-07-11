#!/usr/bin/env python3
"""Run the four policies and plot measured reference errors and energy ratios."""
import pathlib, re, subprocess, sys
ROOT=pathlib.Path(__file__).resolve().parents[2]; HERE=pathlib.Path(__file__).resolve().parent
sys.path.insert(0,str(ROOT/"examples")); from plot_png import BLACK, BLUE, RED, GREEN, Canvas
out=subprocess.run(["cargo","run","--quiet","--example","oscillator_coupling_schemes"],cwd=ROOT,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,check=True).stdout
rows=re.findall(r"^(explicit/CSS|implicit Picard|relaxed implicit|adaptive retry).*energy_ratio=([0-9.eE+-]+).*error_to_reference=([0-9.eE+-]+)",out,re.M)
if len(rows)!=4: raise SystemExit(out)
(HERE/"plots").mkdir(exist_ok=True); c=Canvas(760,430); c.text(35,30,"COUPLING POLICIES: MEASURED ERROR AND ENERGY",BLACK,3); c.line(70,330,710,330,BLACK); c.line(70,80,70,330,BLACK)
for i,(name,en,er) in enumerate(rows):
 x=110+i*150; error=float(er); height=min(220,int(220*error/(max(float(z[2]) for z in rows) or 1))); c.rect(x,330-height,x+42,330,RED); e=float(en); eh=min(220,int(220*min(e,3)/3)); c.rect(x+50,330-eh,x+92,330,BLUE); c.text(x,355,name.replace(" ","\\n"),BLACK,1); c.text(x,70,f"err {error:.2e}",GREEN,1)
c.text(90,405,"RED: error to refined monolithic reference; BLUE: final/initial energy. PASS: adaptive retries and converges.",BLACK,1); c.save(HERE/"plots"/"coupling_schemes.png")
print(out,end="")
