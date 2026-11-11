import { createEffect, createSignal, For, onCleanup, onMount } from 'solid-js';
import { setStats, toggleStatsPin } from '../actions';
import { cv, focusScreen } from '../screen';
import { cfg, live, sleeping, statsMode, statsPinned } from '../state';
import { L, opened, RATE_SECONDS, rate, S, shown } from '../stats';
import { bytes, duration, ms, perSec, store } from '../util';
import { Icon } from './Icons';

/** Title bar height: the part of the panel always kept inside the window. */
const HEAD = 40;

/** A value with its unit set smaller, for the stat tiles. */
function Figure(props: { text: string }) {
  const parts = () => props.text.split(' ');
  return <>{parts()[0]}<small>{parts().slice(1).join(' ')}</small></>;
}

function niceScale(peakKb: number) {
  const [unit, div] = peakKb >= 1024 ? ['MB/s', 1024] : ['KB/s', 1];
  const v = Math.max(peakKb / div, unit === 'KB/s' ? 10 : 1);
  const p = 10 ** Math.floor(Math.log10(v));
  const top = [1, 1.2, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10].map(s => s * p).find(s => s >= v)!;
  return { max: top * div, label: (n: number) => String(+(n / div).toFixed(2)), unit };
}

export function Stats() {
  let box!: HTMLDivElement, chart!: HTMLCanvasElement, tip!: HTMLDivElement;

  // ---- position ----
  // Dragged by its title bar. The position is remembered; it is kept inside
  // the window (at least the title bar) without being overwritten by that.
  let pos = (() => {
    try {
      const p = JSON.parse(store.get('stats-pos') ?? '');
      if (Number.isFinite(p.x) && Number.isFinite(p.y)) return p as { x: number; y: number };
    } catch {}
    return { x: 8, y: 56 };
  })();
  function place() {
    const x = Math.min(Math.max(pos.x, 0), Math.max(innerWidth - (box.offsetWidth || 320), 0));
    const y = Math.min(Math.max(pos.y, 0), Math.max(innerHeight - HEAD, 0));
    box.style.left = x + 'px';
    box.style.top = y + 'px';
    box.style.maxHeight = Math.max(innerHeight - y - 8, HEAD) + 'px';
  }

  // While dragging, the window takes every pointer event first (capture phase)
  // and stops it there, so none reaches the remote screen underneath, even
  // when the pointer outruns the panel.
  const [dragging, setDragging] = createSignal(false);
  let drag: { id: number; dx: number; dy: number } | null = null;
  function startDrag(e: PointerEvent) {
    if (e.button !== 0 || (e.target as Element).closest('button')) return;
    const r = box.getBoundingClientRect();
    drag = { id: e.pointerId, dx: e.clientX - r.left, dy: e.clientY - r.top };
    try { (e.currentTarget as Element).setPointerCapture(e.pointerId); } catch {}
    setDragging(true);
    e.preventDefault();
  }
  const moveDrag = (e: PointerEvent) => {
    if (!drag || e.pointerId !== drag.id) return;
    e.stopPropagation();
    pos = { x: e.clientX - drag.dx, y: e.clientY - drag.dy };
    place();
  };
  const endDrag = (e: PointerEvent) => {
    if (!drag || e.pointerId !== drag.id) return;
    e.stopPropagation();
    drag = null;
    setDragging(false);
    const r = box.getBoundingClientRect();
    pos = { x: Math.round(r.left), y: Math.round(r.top) };
    store.set('stats-pos', JSON.stringify(pos));
    focusScreen();
  };
  const onResize = () => { place(); drawChart(); };
  onMount(() => {
    addEventListener('pointermove', moveDrag, true);
    addEventListener('pointerup', endDrag, true);
    addEventListener('pointercancel', endDrag, true);
    addEventListener('resize', onResize);
  });
  onCleanup(() => {
    removeEventListener('pointermove', moveDrag, true);
    removeEventListener('pointerup', endDrag, true);
    removeEventListener('pointercancel', endDrag, true);
    removeEventListener('resize', onResize);
  });

  // ---- figures ----
  const total = () => {
    shown();
    return L.host != null && L.rtt != null && L.client != null ? L.host + L.rtt / 2 + L.client : null;
  };
  const latencyParts = () => { shown(); return [L.host, L.rtt == null ? null : L.rtt / 2, L.client]; };
  const average = () => { shown(); return rate.length ? rate.reduce((a, b) => a + b, 0) / rate.length : 0; };
  const peak = () => { shown(); return rate.length ? Math.max(...rate) : 0; };
  const scale = () => niceScale(peak());
  const details = () => {
    const s = shown();
    const state = !live() ? 'offline' : sleeping() ? 'asleep' : document.hidden ? 'tab hidden' : 'live';
    return [
      ['Frames', s.fps ? `${s.fps}/s, ${s.rects.toFixed(1)} rectangles each` : 'none, screen unchanged'],
      ['Last frame', S.lastSize ? `${bytes(S.lastSize)}, ${S.lastKind}` : '–'],
      ['Sending', `${perSec(s.kbOut)} (input and acks)`],
      ['Host', `${ms(L.host)} from capture to send`],
      ['Network', `${ms(L.rtt)} round trip, half counted`],
      ['Browser', `${ms(L.client)} from receive to draw`],
      ['Stream', `${cv.width} × ${cv.height}, ${cfg.quality} quality, ${cfg.soft ? 'soft' : 'sharp'} motion, ≤ ${cfg.fps} fps`],
      ['State', state],
    ];
  };

  // ---- chart ----
  // Bar chart of KB/s received per second. One series, so no legend: the
  // section title names it. Hovering a bar shows its value.
  const [hoverX, setHoverX] = createSignal<number | null>(null);
  function geometry() {
    const w = chart.clientWidth, h = chart.clientHeight, gutter = 34;
    return { w, h, gutter, top: 6, bottom: h - 1, slot: (w - gutter) / RATE_SECONDS };
  }
  function hoveredBar() {
    const x = hoverX();
    if (x == null) return -1;
    const { gutter, slot } = geometry();
    const i = Math.floor((x - gutter) / slot) - (RATE_SECONDS - rate.length);
    return i >= 0 && i < rate.length ? i : -1;
  }
  function drawChart() {
    if (statsMode() !== 'open') return;   // (and a hidden body draws nothing: its width is 0)
    const { w, h, gutter, top, bottom, slot } = geometry();
    if (!w) return;
    const dpr = devicePixelRatio || 1;
    if (chart.width !== Math.round(w * dpr) || chart.height !== Math.round(h * dpr)) {
      chart.width = Math.round(w * dpr); chart.height = Math.round(h * dpr);
    }
    const g = chart.getContext('2d')!;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, w, h);
    const css = getComputedStyle(document.documentElement);
    const color = (name: string) => css.getPropertyValue(name).trim();
    const sc = scale();
    const y = (v: number) => bottom - (v / sc.max) * (bottom - top);

    // Gridlines (hairline) with clean tick labels in the left gutter.
    g.font = `11px ${color('--sans')}`; g.textAlign = 'right'; g.textBaseline = 'middle';
    for (const v of [0, sc.max / 2, sc.max]) {
      const yy = Math.round(y(v)) + 0.5;
      g.strokeStyle = v === 0 ? color('--axis') : color('--grid'); g.lineWidth = 1;
      g.beginPath(); g.moveTo(gutter, yy); g.lineTo(w, yy); g.stroke();
      g.fillStyle = color('--muted');
      g.fillText(sc.label(v), gutter - 6, Math.min(Math.max(yy, 6), h - 6));
    }

    // Bars: newest on the right, 2px gap between neighbours, rounded tops.
    const hovered = hoveredBar();
    const bw = Math.max(1, Math.min(24, slot - 2));
    rate.forEach((v, i) => {
      if (v <= 0) return;
      const x = gutter + (RATE_SECONDS - rate.length + i) * slot + (slot - bw) / 2;
      const barTop = Math.min(y(v), bottom - 1);
      const r = Math.min(4, bw / 2, bottom - barTop);
      g.fillStyle = i === hovered ? color('--s1-hover') : color('--s1');
      g.beginPath(); g.roundRect(x, barTop, bw, bottom - barTop, [r, r, 0, 0]); g.fill();
    });
    showTip(hovered, hovered >= 0 ? y(rate[hovered]) : 0);
  }
  // The tooltip sits just above the hovered bar, kept inside the chart.
  function showTip(i: number, barTop: number) {
    if (i < 0) { tip.style.display = 'none'; return; }
    const { gutter, slot } = geometry();
    const ago = rate.length - 1 - i;
    const b = document.createElement('b'), span = document.createElement('span');
    b.textContent = perSec(rate[i]);
    span.textContent = ago ? `${ago} s ago` : 'last second';
    tip.replaceChildren(b, span);
    const x = gutter + (RATE_SECONDS - rate.length + i + 0.5) * slot;
    tip.style.display = 'block';
    tip.style.left = Math.min(Math.max(x, 44), chart.clientWidth - 44) + 'px';
    tip.style.top = Math.max(barTop - 6, tip.offsetHeight) + 'px';
  }

  // Redraw every second, on hover, and when the panel opens or is pinned.
  createEffect(() => { shown(); hoverX(); statsPinned(); if (statsMode() === 'open') { place(); drawChart(); } });

  return (
    <div id="stats" class="panel" ref={box}
      classList={{ show: statsMode() !== 'off', pinned: statsPinned(), dragging: dragging() }}
      // Opening on hover: draw at once rather than at the next one-second tick.
      onMouseEnter={drawChart}>
      <div id="stats-head" onPointerDown={startDrag}>
        <span id="stats-title">Stats</span>
        <span id="stats-sum">{`${shown().fps} fps · ${perSec(shown().kbIn)} · ${ms(total())}`}</span>
        <button id="b-stats-pin" class="tb icon" tabIndex={-1} aria-pressed={statsPinned()}
          title={statsPinned() ? 'Unpin' : 'Pin'} onClick={toggleStatsPin}><Icon name="pin" /></button>
        <button id="b-close" class="tb icon" tabIndex={-1} title="Close"
          onClick={() => { setStats('off'); focusScreen(); }}><Icon name="x" /></button>
      </div>
      <div id="stats-body">
        <div class="tiles">
          <div class="tile"><div class="k">Frame rate</div><div class="v"><Figure text={`${shown().fps} fps`} /></div></div>
          <div class="tile"><div class="k">Receiving</div><div class="v"><Figure text={perSec(shown().kbIn)} /></div></div>
          <div class="tile"><div class="k">Latency</div><div class="v">
            <Figure text={total() == null ? '– ms' : ms(total())} /></div></div>
        </div>

        <div class="section">
          <div class="section-title">
            <span>{`Received per second, ${scale().unit}`}</span><span class="aside">last 60 s</span>
          </div>
          <div id="chart-wrap">
            <canvas id="chart" ref={chart}
              onPointerMove={e => setHoverX(e.clientX - chart.getBoundingClientRect().left)}
              onPointerLeave={() => setHoverX(null)} />
            <div id="tip" ref={tip} />
          </div>
          <div class="chart-foot">
            <span>Average <b>{perSec(average())}</b></span><span>Peak <b>{perSec(peak())}</b></span>
          </div>
        </div>

        <div class="section">
          <div class="section-title"><span>Latency</span><span class="aside">screen change → drawn here</span></div>
          <div id="lat-bar">
            <For each={['--s1', '--s2', '--s3']}>{(c, i) =>
              <div style={{ background: `var(${c})`, flex: `${total() ? (latencyParts()[i()] ?? 0) / total()! : 0} 1 0` }} />
            }</For>
          </div>
          <div class="legend">
            <span><i style={{ background: 'var(--s1)' }} />Host <b>{ms(latencyParts()[0])}</b></span>
            <span><i style={{ background: 'var(--s2)' }} />Network <b>{ms(latencyParts()[1])}</b></span>
            <span><i style={{ background: 'var(--s3)' }} />Browser <b>{ms(latencyParts()[2])}</b></span>
          </div>
        </div>

        <div class="section">
          <div class="section-title"><span>This session</span><span class="aside">resets on reload</span></div>
          <div class="session">
            <div><div class="k">Received</div><div class="v">{(shown(), bytes(S.totalIn))}</div></div>
            <div><div class="k">Sent</div><div class="v">{(shown(), bytes(S.totalOut))}</div></div>
            <div><div class="k">Open for</div><div class="v">{(shown(), duration(performance.now() - opened))}</div></div>
          </div>
        </div>

        <details>
          <summary><Icon name="chev" />Details</summary>
          <table id="details"><tbody>
            <For each={details()}>{([k, v]) => <tr><td>{k}</td><td>{v}</td></tr>}</For>
          </tbody></table>
        </details>
      </div>
    </div>
  );
}
