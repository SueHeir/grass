#!/usr/bin/env python3
"""Run the four policies and plot measured reference errors and energy ratios."""
import pathlib, re, subprocess, sys
ROOT=pathlib.Path(__file__).resolve().parents[2]; HERE=pathlib.Path(__file__).resolve().parent
sys.path.insert(0,str(ROOT/"examples")); from plot_png import BLACK, BLUE, RED, GREEN, Canvas
out=subprocess.run(["cargo","run","--quiet","--example","oscillator_coupling_schemes"],cwd=ROOT,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,check=True).stdout
rows=re.findall(r"^(explicit/CSS|implicit Picard|relaxed implicit|adaptive retry).*energy_ratio=([0-9.eE+-]+).*error_to_exact=([0-9.eE+-]+).*error_to_nominal_monolithic=([0-9.eE+-]+)",out,re.M)
if len(rows)!=4: raise SystemExit(out)
(HERE/"plots").mkdir(exist_ok=True); c=Canvas(760,430); c.text(35,30,"COUPLING POLICIES: MEASURED ERROR AND ENERGY",BLACK,3); c.line(70,330,710,330,BLACK); c.line(70,80,70,330,BLACK)
for i,(name,en,exact,monolithic) in enumerate(rows):
 x=110+i*150; error=float(exact); log_error=max(-8,min(0,__import__('math').log10(max(error,1e-8)))); height=int(220*(log_error+8)/8); c.rect(x,330-height,x+42,330,RED); e=float(en); eh=min(220,int(220*min(e,1.2)/1.2)); c.rect(x+50,330-eh,x+92,330,BLUE); c.text(x,355,name.replace(" ","\\n"),BLACK,1); c.text(x,70,f"exact {error:.1e}",GREEN,1)
bound_y=330-int(220*(__import__('math').log10(4e-1)+8)/8)
c.line(70,bound_y,710,bound_y,GREEN); c.text(75,bound_y-10,"PASS: exact-mode error <= 4e-1",GREEN,1)
c.text(8,90,"1e0",BLACK,1); c.text(8,310,"1e-8",BLACK,1)
c.text(80,405,"RED: error to exact antisymmetric normal mode (log scale); BLUE: final/initial energy. Adaptive retries to 0.001.",BLACK,1); c.save(HERE/"plots"/"coupling_schemes.png")
print(out,end="")
