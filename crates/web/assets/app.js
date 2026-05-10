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
