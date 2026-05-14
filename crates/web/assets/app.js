const CSRF_COOKIE_RE = /(?:^|; )tw1337_csrf=([^;]+)/;

document.body.addEventListener('htmx:configRequest', (evt) => {
  if (evt.detail.verb !== 'get') {
    const m = document.cookie.match(CSRF_COOKIE_RE);
    if (m) evt.detail.headers['X-Csrf-Token'] = decodeURIComponent(m[1]);
  }
});

(function berlinTimer() {
  const root = document.getElementById('berlin-timer');
  if (!root) return;
  const labelEl = root.querySelector('.timer-label');
  const clockEl = root.querySelector('.timer-clock');
  if (!labelEl || !clockEl) return;

  const fmt = new Intl.DateTimeFormat('en-GB', {
    timeZone: 'Europe/Berlin',
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });

  function berlinHMS(d) {
    const parts = fmt.formatToParts(d);
    const get = (t) => parts.find((p) => p.type === t).value;
    return [+get('hour'), +get('minute'), +get('second')];
  }

  function pad(n) {
    return String(n).padStart(2, '0');
  }

  function tick() {
    const [h, m, s] = berlinHMS(new Date());
    const armed = h === 13 && m === 37;
    if (armed) {
      root.classList.add('armed');
      labelEl.textContent = '1337 ARMED';
      clockEl.textContent = `13:37:${pad(s)}`;
    } else {
      root.classList.remove('armed');
      const nowSec = h * 3600 + m * 60 + s;
      const targetSec = 13 * 3600 + 37 * 60;
      let delta = targetSec - nowSec;
      if (delta <= 0) delta += 86400;
      const dh = Math.floor(delta / 3600);
      const dm = Math.floor((delta % 3600) / 60);
      const ds = delta % 60;
      labelEl.textContent = 'T-MINUS';
      clockEl.textContent = `${pad(dh)}:${pad(dm)}:${pad(ds)}`;
    }
  }
  tick();
  setInterval(tick, 1000);
})();

document.addEventListener(
  'keydown',
  (evt) => {
    if (evt.key !== '/' || evt.metaKey || evt.ctrlKey || evt.altKey) return;
    const tag = (evt.target && evt.target.tagName) || '';
    if (tag === 'INPUT' || tag === 'TEXTAREA' || (evt.target && evt.target.isContentEditable)) {
      return;
    }
    const search = document.getElementById('page-search');
    if (search) {
      evt.preventDefault();
      search.focus();
      search.select();
    }
  },
  true,
);

(function liveFilter() {
  const search = document.getElementById('page-search');
  if (!search) return;
  const items = Array.from(document.querySelectorAll('[data-filter]')).map((el) => ({
    el,
    hay: (el.getAttribute('data-filter') || '').toLowerCase(),
  }));
  if (items.length === 0) return;
  search.addEventListener('input', () => {
    const q = search.value.trim().toLowerCase();
    for (const { el, hay } of items) {
      el.style.display = !q || hay.includes(q) ? '' : 'none';
    }
  });
})();

(function settingsPage() {
  const form = document.getElementById('settings-form');
  const saveBar = document.getElementById('settings-save-bar');
  if (!form || !saveBar) return;

  const countEl = saveBar.querySelector('[data-count]');
  const nounEl = saveBar.querySelector('[data-noun]');
  const previewEl = saveBar.querySelector('[data-preview]');
  const discardBtn = saveBar.querySelector('[data-discard]');

  const rows = Array.from(form.querySelectorAll('.settings-row'))
    .map((row) => ({
      el: row,
      input: row.querySelector('input[name]'),
      reset: row.querySelector('.row-reset'),
      pretty: row.querySelector('.row-pretty'),
      section: row.dataset.section,
    }))
    .filter((r) => r.input);

  function formatPretty(input, prettyEl) {
    const unit = prettyEl?.dataset.prettyUnit ?? '';
    if (unit === 'bool') return input.checked ? 'On' : 'Off';
    const n = Number(input.value);
    if (!Number.isFinite(n)) return input.value;
    if (unit === 's') return n === 1 ? '1 second' : `${n} seconds`;
    if (unit === 'B') {
      if (n % 1024 === 0 && n >= 1024) return `${n / 1024} KiB`;
      return `${n} B`;
    }
    return unit ? `${n} ${unit}` : String(n);
  }
  const cards = Array.from(form.querySelectorAll('.settings-card'));
  const navItems = Array.from(document.querySelectorAll('.settings-nav-item'));

  function currentValue(input) {
    return input.type === 'checkbox' ? (input.checked ? 'true' : 'false') : input.value;
  }

  function applyDefault(input) {
    const def = input.dataset.default ?? '';
    if (input.type === 'checkbox') {
      input.checked = def === 'true' || def === '1';
    } else {
      input.value = def;
    }
  }

  function refreshAll() {
    const dirtyKeys = [];
    const perSection = new Map();
    for (const r of rows) {
      const dirty = currentValue(r.input) !== (r.input.dataset.default ?? '');
      r.el.classList.toggle('is-dirty', dirty);
      if (r.reset) r.reset.hidden = !dirty;
      if (r.pretty) r.pretty.textContent = formatPretty(r.input, r.pretty);
      if (!dirty) continue;
      dirtyKeys.push(r.input.dataset.key ?? r.input.name);
      if (r.section) perSection.set(r.section, (perSection.get(r.section) ?? 0) + 1);
    }

    for (const card of cards) {
      const n = perSection.get(card.dataset.section) ?? 0;
      const badge = card.querySelector('.card-dirty');
      if (badge) {
        badge.hidden = n === 0;
        badge.textContent = `${n} modified`;
      }
    }
    for (const item of navItems) {
      const n = perSection.get(item.dataset.target) ?? 0;
      const badge = item.querySelector('.ndirty');
      if (badge) {
        badge.hidden = n === 0;
        badge.textContent = String(n);
      }
    }

    const total = dirtyKeys.length;
    countEl.textContent = String(total);
    nounEl.textContent = total === 1 ? 'change' : 'changes';
    const preview = dirtyKeys.slice(0, 3).join(' · ');
    previewEl.textContent =
      total > 3 ? `${preview} +${total - 3}` : preview;
    saveBar.classList.toggle('visible', total > 0);
  }

  form.addEventListener('input', refreshAll);
  form.addEventListener('change', refreshAll);

  form.addEventListener('click', (e) => {
    const btn = e.target.closest('.row-reset');
    if (!btn) return;
    const r = rows.find((row) => row.reset === btn);
    if (!r) return;
    applyDefault(r.input);
    refreshAll();
    r.input.focus();
  });

  discardBtn?.addEventListener('click', () => location.reload());

  if (cards.length && 'IntersectionObserver' in window) {
    const byId = new Map(navItems.map((el) => [el.dataset.target, el]));
    let active = null;
    const setActive = (section) => {
      if (section === active) return;
      if (active) byId.get(active)?.classList.remove('active');
      active = section;
      byId.get(section)?.classList.add('active');
    };
    setActive(cards[0].dataset.section);
    const io = new IntersectionObserver(
      (entries) => {
        const vis = entries
          .filter((e) => e.isIntersecting)
          .sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top)[0];
        if (vis) setActive(vis.target.dataset.section);
      },
      { rootMargin: '-72px 0px -60% 0px' },
    );
    for (const card of cards) io.observe(card);
  }

  for (const item of navItems) {
    item.addEventListener('click', (e) => {
      const target = document.getElementById('sec-' + item.dataset.target);
      if (!target) return;
      e.preventDefault();
      target.scrollIntoView({ behavior: 'smooth', block: 'start' });
    });
  }

  refreshAll();
})();
