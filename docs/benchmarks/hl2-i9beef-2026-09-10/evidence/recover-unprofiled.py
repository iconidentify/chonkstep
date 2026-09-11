import subprocess,os,time,pathlib,json
r=pathlib.Path(__file__).parent
e=dict(os.environ,DISPLAY=':1',WAYLAND_DISPLAY='wayland-1',HYPRLAND_INSTANCE_SIGNATURE='chonkstep_1789069219_137376')
pid=1072684
records=[]
def ctl(*args):return subprocess.check_output(['hyprctl',*args],env=e,text=True)
def state(phase):
 c=next(c for c in json.loads(ctl('-j','clients')) if c['pid']==pid)
 a=json.loads(ctl('-j','activewindow'))
 d={'phase':phase,'monotonic':time.monotonic(),'game':c,'focused':a.get('pid')};records.append(d)
 (r/'unprofiled-recovery.json').write_text(json.dumps(records,indent=2));return c,a
c,a=state('before');assert a['pid']==pid
for i in range(3):
 ctl('dispatch','focuswindow','pid:1018448');time.sleep(1)
 c,a=state(f'focus-away-{i}');assert a['pid']==1018448
 ctl('dispatch','focuswindow',f'pid:{pid}');time.sleep(2)
 c,a=state(f'focus-return-{i}');assert a['pid']==pid and c['fullscreen']==1
subprocess.run(['xdotool','key','shift+Tab'],env=e,check=True);time.sleep(2)
subprocess.run(['grim','-o','DP-1',str(r/'unprofiled-steam-overlay.png')],env=e,check=True)
state('overlay-open-key')
subprocess.run(['xdotool','key','shift+Tab'],env=e,check=True);time.sleep(2)
state('overlay-close-key')
ctl('dispatch','movetoworkspacesilent',f'2,address:{c["address"]}');time.sleep(2)
c,a=state('space-parked');assert c['workspace']['id']==2 and a['pid']!=pid
ctl('dispatch','movetoworkspacesilent',f'1,address:{c["address"]}')
ctl('dispatch','focuswindow',f'pid:{pid}');time.sleep(3)
c,a=state('space-return');assert c['workspace']['id']==1 and a['pid']==pid and c['fullscreen']==1
print('Verified focus and Space transitions, same process',pid)
subprocess.run(['python',str(r/'measure.py'),'vulkan-unprofiled-after-recovery','30'],check=True,env=e)
