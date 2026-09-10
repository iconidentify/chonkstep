import pathlib,subprocess,socket,json,time,os,sys,re
root=pathlib.Path(__file__).parent
out=root/sys.argv[1];out.mkdir(exist_ok=False)
seconds=float(sys.argv[2]) if len(sys.argv)>2 else 20

def diag():
    with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as s:
        s.settimeout(10);s.connect('/run/user/1000/chonkstep/control-wayland-1.sock');s.sendall(b'{"request":"debug","topic":"scene"}\n')
        for line in s.makefile('rb'):
            event=json.loads(line)
            if event.get('event')=='debug': return event['data']
    raise RuntimeError('missing diagnostic reply')

def ticks(pid):
    s=pathlib.Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split();return int(s[11])+int(s[12])

def counters(text):
    line=next(l for l in text.splitlines() if l.startswith('native_pipeline '))
    return {k:int(v) for k,v in re.findall(r'(\w+)=(\d+)',line)}

pid=int(subprocess.check_output(['pgrep','-x','hl2_linux'],text=True).strip())
with (out/'gpu.csv').open('w') as f:
    gpu=subprocess.Popen(['nvidia-smi','--query-gpu=timestamp,index,pstate,utilization.gpu,memory.used,power.draw,clocks.gr,clocks.mem','--format=csv','-l','1'],stdout=f)
    try:
        a=diag();(out/'before.txt').write_text(a);before={p:ticks(p) for p in [137376,pid]};start=time.monotonic();time.sleep(seconds)
        elapsed=time.monotonic()-start;after={p:ticks(p) for p in before};b=diag();(out/'after.txt').write_text(b)
        result={'elapsed':elapsed,'game_pid':pid,'cpu_percent':{p:(after[p]-v)/os.sysconf('SC_CLK_TCK')/elapsed*100 for p,v in before.items()},'native_delta':{k:v-counters(a)[k] for k,v in counters(b).items()}}
        result['compositor_flips_per_second']=result['native_delta']['flips']/elapsed
        (out/'result.json').write_text(json.dumps(result,indent=2));print(json.dumps(result,indent=2),flush=True)
    finally:gpu.terminate();gpu.wait()
subprocess.run(['grim','-o','DP-1',str(out/'screen.png')],env=dict(os.environ,WAYLAND_DISPLAY='wayland-1'),check=True)
