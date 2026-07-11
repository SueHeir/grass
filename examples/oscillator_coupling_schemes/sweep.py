#!/usr/bin/env python3
"""Run the four policies and plot monolithic errors and energy ratios."""
import pathlib, re, subprocess, sys
ROOT=pathlib.Path(__file__).resolve().parents[2]; HERE=pathlib.Path(__file__).resolve().parent
sys.path.insert(0,str(ROOT/"examples")); from plot_png import BLACK, BLUE, RED, GREEN, Canvas
out=subprocess.run(["cargo","run","--quiet","--example","oscillator_coupling_schemes"],cwd=ROOT,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,check=True).stdout
rows=re.findall(r"^(explicit/CSS|implicit Picard|relaxed implicit|adaptive retry).*energy_ratio=([0-9.eE+-]+).*error_to_exact=([0-9.eE+-]+).*error_to_nominal_monolithic=([0-9.eE+-]+)",out,re.M)
if len(rows)!=4: raise SystemExit(out)
(HERE/"plots").mkdir(exist_ok=True); c=Canvas(760,430); c.text(35,30,"COUPLING POLICIES: MONOLITHIC ERROR AND ENERGY",BLACK,3); c.line(70,330,710,330,BLACK); c.line(70,80,70,330,BLACK)
for i,(name,en,exact,monolithic) in enumerate(rows):
 x=110+i*150; error=float(monolithic); log_error=max(-16,min(2,__import__('math').log10(max(error,1e-16)))); height=int(220*(log_error+16)/18); c.rect(x,330-height,x+42,330,RED); e=float(en); eh=min(220,int(220*min(e,1.2)/1.2)); c.rect(x+50,330-eh,x+92,330,BLUE); c.text(x,355,name.replace(" ","\\n"),BLACK,1); c.text(x,70,f"mono {error:.1e}",GREEN,1)
bound_y=330-int(220*(__import__('math').log10(2e-7)+16)/18)
c.line(70,bound_y,710,bound_y,GREEN); c.text(75,bound_y-10,"PASS: converged policies <= 2e-7",GREEN,1)
c.text(8,90,"1e2",BLACK,1); c.text(8,310,"1e-16",BLACK,1)
c.text(80,405,"RED: error to same-window monolithic solve (log); BLUE: final/initial energy. Adaptive retries 0.08 -> 0.04 -> 0.02.",BLACK,1); c.save(HERE/"plots"/"coupling_schemes.png")
print(out,end="")
