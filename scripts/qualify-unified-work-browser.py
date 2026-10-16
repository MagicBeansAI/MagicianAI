#!/usr/bin/env python3
"""Exercise real Work layouts with isolated, in-memory API fixtures via agent-browser."""
import argparse
import base64
import json
from pathlib import Path
import re
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
UI = ROOT / 'ui/unified-ui'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--origin', default='http://127.0.0.1:5175')
    parser.add_argument('--route', action='append', help='Qualify these specific routes instead of the core matrix.')
    parser.add_argument('--session', default='unified-work-qualification')
    parser.add_argument('--output', default=str(ROOT / '.local/designs/unified-work-browser-results.json'))
    args = parser.parse_args()
    if not re.fullmatch(r'http://(?:127\.0\.0\.1|localhost):\d+', args.origin):
        parser.error('Use an isolated localhost development server.')

    def browser(*command):
        result = subprocess.run(['agent-browser', '--session', args.session, *map(str, command)],
                                check=True, text=True, capture_output=True)
        return result.stdout.strip()

    themes_source = (UI / 'src/lib/shared/stores/themeStore.ts').read_text()
    themes = re.findall(r"^  '([a-z0-9-]+)'", themes_source.split('] as const;')[0], re.M)
    if len(themes) != 24:
        raise RuntimeError('Review the theme registry before changing this qualification matrix.')
    fixture = UI / 'scripts/work-browser-fixture.js'
    browser('open', args.origin + '/work', '--init-script', fixture)
    browser('errors', '--clear')
    routes = ['/work', '/work?selected=redirect', '/work?selected=redirect&plan=open', '/work?compose=1', '/work?view=crew',
              '/work?view=crew&agent=forge', '/work?view=programs&program=launch_readiness.md',
              '/work?view=quality&task=brief', '/work?view=needs-you',
              '/work?type=monitors', '/work?type=internal', '/work?view=operations',
              '/work?thread=general', '/work?thread=general&compose=1',
              '/work/crew/new', '/work/crew/forge/edit?from=%2Fwork%3Fview%3Dcrew%26agent%3Dforge',
              '/work/crew/forge/rules', '/work/crew/forge/memory/goal?goal_id=launch']
    if args.route:
        if any(not route.startswith('/') or route.startswith('//') for route in args.route):
            parser.error('Routes must be local paths.')
        routes = args.route
    report = {'fixture': str(fixture.relative_to(ROOT)), 'origin': args.origin,
              'themes': themes, 'results': [], 'failures': [], 'started_at': time.time(),
              'qualification': 'Fixture-backed UI layout; no live-provider or native qualification.'}
    for width, height, text_scale in [(320, 740, 1), (375, 812, 1), (430, 932, 1), (768, 1024, 1), (959, 900, 1),
                                      (960, 900, 1), (1280, 900, 1),
                                      (320, 740, 1.3), (390, 844, 1.3)]:
        for route in routes:
            browser('open', args.origin + route)
            browser('set', 'viewport', width, height)
            # Poll visible DOM readiness, rather than racing the mounted workspace.
            expected = '.plan-inspector-sheet .node-group' if 'plan=open' in route else '.task-panel' if 'selected=' in route else '.task-create-button--primary' if 'compose=' in route else '.work-workspace' if route.startswith('/work') else '.work-specialist-shell'
            ready_script = """(async()=>{for(let n=0;n<100;n++) {if(document.querySelector(EXPECTED) && !document.querySelector('.loading-overlay')) {await document.fonts.ready;await new Promise(r=>setTimeout(r,350));return true;}await new Promise(r=>setTimeout(r,50));}throw new Error('Required Work view did not mount');})()""".replace('EXPECTED', json.dumps(expected))
            browser('eval', '-b', base64.b64encode(ready_script.encode()).decode())
            if route == '/work':
                snapshot = browser('snapshot', '-i')
                trigger = re.search(r'button "More ▾"[^\n]*ref=(e\d+)', snapshot)
                if not trigger:
                    raise RuntimeError('Required More navigation did not mount.')
                browser('click', '@' + trigger.group(1))
            script = """(async()=>{
const {themeStore}=await import('/src/lib/shared/stores/themeStore.ts');
const themes=THEMES, saved=document.documentElement.dataset.theme, rows=[];
document.documentElement.style.fontSize=TEXT_SCALE*100+'%';
function clippedControls() {
 const failures=[];
 for(const element of document.querySelectorAll('.work-workspace button,.work-workspace a,.task-panel button,.task-panel a,.work-specialist-shell button,.work-specialist-shell a')) {
  const rect=element.getBoundingClientRect(), style=getComputedStyle(element);
  if(!rect.width||!rect.height||rect.bottom<=0||rect.top>=innerHeight||style.visibility==='hidden')continue;
  let scrollX=false,scrollY=false;
  for(let parent=element.parentElement;parent&&parent!==document.body;parent=parent.parentElement) {
   const css=getComputedStyle(parent),box=parent.getBoundingClientRect();
   scrollX ||= /auto|scroll/.test(css.overflowX); scrollY ||= /auto|scroll/.test(css.overflowY);
   const dx=!scrollX&&/hidden|clip/.test(css.overflowX)?Math.max(0,box.left-rect.left,rect.right-box.right):0;
   const dy=!scrollY&&/hidden|clip/.test(css.overflowY)?Math.max(0,box.top-rect.top,rect.bottom-box.bottom):0;
   if(Math.max(dx,dy)>1){failures.push({label:element.getAttribute('aria-label')||element.textContent.trim().slice(0,80),container:parent.className,clipped:Math.max(dx,dy)});break;}
   // These overlays use the viewport as their containing block. Their
   // hidden layout ancestors do not clip fixed content; inspect the overlay
   // itself and stop at its fixed boundary.
   if(css.position==='fixed')break;
  }
 }
 return failures;
}
try { for(const theme of themes) {
 themeStore.applyRemoteTheme(theme);
 const fonts=document.getElementById('magican-costume-fonts');
 if(fonts&&!fonts.sheet)await new Promise(resolve=>{fonts.addEventListener('load',resolve,{once:true});fonts.addEventListener('error',resolve,{once:true});setTimeout(resolve,5000)});
 await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));
 await document.fonts.ready;
 const root=document.documentElement,body=document.querySelector('.work-body')||document.querySelector('.work-specialist-body')||document.body,panel=document.querySelector('.task-panel'),box=panel?.getBoundingClientRect(),deep=document.querySelector('.plan-inspector-sheet')?.getBoundingClientRect(),menu=document.querySelector('.more-menu')?.getBoundingClientRect();
 const tokens=getComputedStyle(root), create=document.querySelector('.work-create');
 const result={theme,width:innerWidth,textScale:TEXT_SCALE,documentOverflow:root.scrollWidth-innerWidth,bodyOverflow:body.scrollWidth-body.clientWidth,panelRole:panel?.getAttribute('role')||null,panelOutside:box ? Math.max(0,-box.left,box.right-innerWidth):0,planInspectorOutside:deep ? Math.max(0,-deep.left,deep.right-innerWidth):0,taskFeedHeight:document.querySelector('.tasks-page-main')?.clientHeight??null,moreMenuOutside:menu ? Math.max(0,-menu.left,menu.right-innerWidth,-menu.top,menu.bottom-innerHeight):0,clippedControls:clippedControls(),missingTokens:['--bg-card','--text-primary','--border-soft','--font-primary','--radius-md','--button-primary-bg','--button-primary-color'].filter(t=>!tokens.getPropertyValue(t).trim()),createHeight:(create||document.querySelector('.task-create-button--primary'))?.getBoundingClientRect().height||0};
 result.passed=result.documentOverflow<=1&&result.bodyOverflow<=1&&result.panelOutside<=1&&result.planInspectorOutside<=1&&result.moreMenuOutside<=1&&(result.taskFeedHeight===null||result.taskFeedHeight>=160)&&!result.clippedControls.length&&!result.missingTokens.length&&(!EXPECT_PANEL || result.panelRole===(innerWidth>=960?'region':'dialog'))&&(!EXPECT_CREATE || result.createHeight>=43 || innerWidth>=960 && !!document.querySelector('.task-create-button--primary'));
 rows.push(result);
} } finally { themeStore.applyRemoteTheme(saved); }
return rows;
})()""".replace('THEMES', json.dumps(themes)).replace('TEXT_SCALE', str(text_scale)).replace('EXPECT_PANEL', 'true' if 'selected=' in route else 'false').replace('EXPECT_CREATE', 'true' if route.startswith('/work') else 'false')
            rows = json.loads(browser('eval', '-b', base64.b64encode(script.encode()).decode()))
            if isinstance(rows, str):
                rows = json.loads(rows)
            for row in rows:
                row['route'] = route
            report['results'].extend(rows)
            report['failures'].extend(row for row in rows if not row['passed'])
            print(f"{width}px text {text_scale:g} {route}: {sum(r['passed'] for r in rows)}/{len(rows)} themes", flush=True)
            # Keep partial evidence if a later browser command fails.
            path = Path(args.output)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps(report, indent=2) + '\n')
    report['finished_at'] = time.time()
    errors = json.loads(browser('errors', '--json'))['data']['errors']
    report['browser_errors'] = sorted(set(error['text'].split('\n\n')[0] for error in errors))
    path.write_text(json.dumps(report, indent=2) + '\n')
    print(f"{len(report['results'])} layout cases, {len(report['failures'])} failures. Evidence: {path}")
    return int(bool(report['failures'] or report['browser_errors']))


if __name__ == '__main__':
    raise SystemExit(main())
