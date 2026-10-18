import { describe, it, expect, vi, beforeEach } from 'vitest';

// The Songs tab, through the panel it builds (init is the way in) plus the
// pure helpers it exports for transposing, rendering and ordering.
const apiCalls = [];
let library, fullSongs, searchReply, fetchReplies;

vi.mock('/static/api.js', () => ({
  api: (path, opts = {}) => {
    apiCalls.push({ path, opts });
    const method = opts.method ?? 'GET';
    if (path === '/chords/songs' && method === 'GET') return Promise.resolve(json(library));
    if (path === '/chords/songs' && method === 'POST') return Promise.resolve(json({ ...opts.body, id: 'new', title: opts.body.song_name, artist: opts.body.artist_name }));
    const m = path.match(/^\/chords\/songs\/([^/?]+)$/);
    if (m && method === 'GET') return Promise.resolve(json(fullSongs[decodeURIComponent(m[1])]));
    if (m) return Promise.resolve(json(fullSongs[decodeURIComponent(m[1])]));
    if (path.startsWith('/chords/search')) return Promise.resolve(json(searchReply));
    if (path.startsWith('/chords/fetch')) {
      const url = decodeURIComponent(path.split('url=')[1]);
      return Promise.resolve(json(fetchReplies[url]));
    }
    return Promise.resolve(json({}));
  },
}));
vi.mock('/static/util.js', async importOriginal => ({ ...(await importOriginal()), showToast: vi.fn() }));

function json(body) {
  return { ok: true, status: 200, json: () => Promise.resolve(body), text: () => Promise.resolve('') };
}

const {
  init, transposeChord, renderSheet, sortSongs, groupByArtist, letterOf, sortKey, youtubeId,
  clampSpeed, isScrolling,
} = await import('/static/chords.js');

const settle = () => new Promise(r => setTimeout(r, 0));

function summary(id, title, artist, extra = {}) {
  return {
    id, title, artist, title_key: sortKey(title), artist_key: sortKey(artist),
    ug_tab_id: null, source_url: null, kind: 'Chords', version: 1, rating: null, votes: null,
    saved_ms: 1, last_played_ms: null, ...extra,
  };
}

function full(s, content = '[Verse]\n[tab][ch]G[/ch]   [ch]D/F#[/ch]\nHello there[/tab]') {
  return { ...s, capo: null, tuning: null, tonality: null, content, transpose: 0, scroll_speed: null, youtube_url: null, notes: null };
}

function hit(tab_id, version, rating) {
  return { tab_id, song_name: 'Pompeii', artist_name: 'Bastille', kind: 'Chords', version, rating, votes: 10,
    tab_url: `https://tabs.ultimate-guitar.com/tab/bastille/pompeii-chords-${tab_id}` };
}

function ugTab(h, content) {
  return { tab_id: h.tab_id, song_name: h.song_name, artist_name: h.artist_name, kind: 'Chords',
    version: h.version, rating: h.rating, votes: 10, capo: 2, url: h.tab_url, content, versions: [] };
}

async function mount() {
  const panel = document.createElement('section');
  panel.className = 'panel active';
  document.body.append(panel);
  init(panel);
  await settle();
  return panel;
}

beforeEach(() => {
  document.body.innerHTML = '';
  document.body.className = '';
  apiCalls.length = 0;
  localStorage.clear();
  const songs = [
    summary('s1', 'Pompeii', 'Bastille'),
    summary('s2', 'Yesterday', 'The Beatles'),
    summary('s3', 'Angel', 'Jimi Hendrix'),
    summary('s4', 'Let It Be', 'The Beatles'),
    summary('s5', 'Clint Eastwood', 'Gorillaz'),
  ];
  library = songs;
  fullSongs = Object.fromEntries(songs.map(s => [s.id, full(s)]));
  const a = hit(1001, 1, 4.8), b = hit(1002, 2, 4.5);
  searchReply = { groups: [{ song_name: 'Pompeii', artist_name: 'Bastille', versions: [a, b] }], saved: {} };
  fetchReplies = {
    [a.tab_url]: { tab: ugTab(a, '[ch]Am[/ch] first version'), saved: {} },
    [b.tab_url]: { tab: ugTab(b, '[ch]C[/ch] second version'), saved: {} },
  };
  window.confirm = () => true;
});

describe('transposeChord', () => {
  it('moves roots and slash basses, keeping the quality', () => {
    expect(transposeChord('G', 2)).toBe('A');
    expect(transposeChord('D/F#', 2)).toBe('E/G#');
    expect(transposeChord('Am7', -1)).toBe('G#m7');
    expect(transposeChord('F#m7b5', 1)).toBe('Gm7b5');
    expect(transposeChord('Cadd9', 0)).toBe('Cadd9');
    expect(transposeChord('Em', 12)).toBe('Em');
  });

  it('keeps flats flat and otherwise says Bb and Eb', () => {
    expect(transposeChord('Db', 2)).toBe('Eb');
    expect(transposeChord('Bb', 1)).toBe('B');
    expect(transposeChord('A', 1)).toBe('Bb');
    expect(transposeChord('C', 1)).toBe('C#');
    expect(transposeChord('D', 1)).toBe('Eb');
  });
});

describe('renderSheet', () => {
  it('marks chords and sections and drops tab markers', () => {
    const html = renderSheet('[Chorus]\n[tab][ch]G[/ch]  [ch]D/F#[/ch]\nSing it[/tab]', 2);
    expect(html).toContain('<span class="songs-sec">Chorus</span>');
    expect(html).toContain('<span class="songs-ch">A</span>');
    expect(html).toContain('<span class="songs-ch">E/G#</span>');
    expect(html).not.toContain('[tab]');
    expect(html).toContain('Sing it');
  });

  it('escapes whatever the sheet contains', () => {
    const html = renderSheet('<img src=x onerror=alert(1)> [ch]G[/ch]');
    expect(html).not.toContain('<img');
    expect(html).toContain('&lt;img');
  });
});

describe('ordering', () => {
  it('files The Beatles under B and sorts A–Z', () => {
    expect(sortKey('The Beatles')).toBe('beatles');
    const titles = sortSongs(library).map(s => s.title);
    expect(titles).toEqual(['Angel', 'Clint Eastwood', 'Let It Be', 'Pompeii', 'Yesterday']);
    const artists = groupByArtist(library).map(g => g.artist);
    expect(artists).toEqual(['Bastille', 'The Beatles', 'Gorillaz', 'Jimi Hendrix']);
    expect(groupByArtist(library)[1].songs.map(s => s.title)).toEqual(['Let It Be', 'Yesterday']);
  });

  it('puts a non-letter under #', () => {
    expect(letterOf('99 problems')).toBe('#');
    expect(letterOf('angel')).toBe('A');
  });
});

describe('youtubeId', () => {
  it('reads every link form and rejects other hosts', () => {
    expect(youtubeId('https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=5')).toBe('dQw4w9WgXcQ');
    expect(youtubeId('https://youtu.be/dQw4w9WgXcQ?si=x')).toBe('dQw4w9WgXcQ');
    expect(youtubeId('https://www.youtube.com/shorts/dQw4w9WgXcQ')).toBe('dQw4w9WgXcQ');
    expect(youtubeId('https://evil.example/watch?v=dQw4w9WgXcQ')).toBeNull();
    expect(youtubeId('javascript:alert(1)')).toBeNull();
    expect(youtubeId('https://youtube.com/watch?v=short')).toBeNull();
  });
});

describe('library', () => {
  it('lists A–Z with letter headings', async () => {
    const panel = await mount();
    const letters = [...panel.querySelectorAll('.songs-letter')].map(h => h.textContent);
    expect(letters).toEqual(['A', 'C', 'L', 'P', 'Y']);
    const rows = [...panel.querySelectorAll('#songs-list .songs-row-title')].map(r => r.textContent);
    expect(rows).toEqual(['Angel', 'Clint Eastwood', 'Let It Be', 'Pompeii', 'Yesterday']);
  });

  it('groups by artist, remembers the choice, and filters', async () => {
    let panel = await mount();
    panel.querySelector('.songs-mode[data-mode="artist"]').click();
    const artists = [...panel.querySelectorAll('.songs-artist span:nth-child(2)')].map(s => s.textContent);
    expect(artists).toEqual(['Bastille', 'The Beatles', 'Gorillaz', 'Jimi Hendrix']);

    panel = await mount();
    expect(panel.querySelector('.songs-mode[data-mode="artist"]').classList.contains('is-active')).toBe(true);

    const filter = panel.querySelector('#songs-filter');
    filter.value = 'beatles';
    filter.dispatchEvent(new Event('input'));
    const titles = [...panel.querySelectorAll('#songs-list .songs-row-title')].map(r => r.textContent);
    expect(titles).toEqual(['Let It Be', 'Yesterday']);
  });

  it('collapses an artist and keeps it collapsed', async () => {
    let panel = await mount();
    panel.querySelector('.songs-mode[data-mode="artist"]').click();
    panel.querySelector('.songs-artist').click();
    expect(panel.querySelector('.songs-artist-body').hidden).toBe(true);
    panel = await mount();
    expect(panel.querySelector('.songs-artist-body').hidden).toBe(true);
  });
});

describe('player', () => {
  async function openSaved(id = 's1') {
    const panel = await mount();
    panel.querySelector(`.songs-row[data-id="${id}"]`).click();
    await settle();
    return panel;
  }

  it('shows the saved sheet, transposed on demand', async () => {
    const panel = await openSaved();
    expect(panel.querySelector('#songs-player').hidden).toBe(false);
    expect(panel.querySelector('#songs-library').hidden).toBe(true);
    const chords = () => [...panel.querySelectorAll('.songs-ch')].map(c => c.textContent);
    expect(chords()).toEqual(['G', 'D/F#']);
    panel.querySelector('#songs-tup').click();
    expect(chords()).toEqual(['G#', 'Eb/G']);
    expect(panel.querySelector('#songs-tval').textContent).toBe('+1');
  });

  const autoscroll = panel => panel.querySelector('#songs-autoscroll');

  it('auto-scroll is off by default: no controls, and taps do nothing', async () => {
    const panel = await openSaved();
    expect(autoscroll(panel).getAttribute('aria-pressed')).toBe('false');
    expect(panel.querySelector('#songs-scrollctl').hidden).toBe(true);
    panel.querySelector('#songs-sheet pre').click();
    expect(isScrolling()).toBe(false);
  });

  it('turning auto-scroll on shows play and speed; tapping the sheet starts and pauses it', async () => {
    const panel = await openSaved();
    autoscroll(panel).click();
    expect(autoscroll(panel).getAttribute('aria-pressed')).toBe('true');
    expect(panel.querySelector('#songs-scrollctl').hidden).toBe(false);
    expect(panel.querySelector('#songs-speed').textContent).toBe('20');
    expect(isScrolling()).toBe(false); // never starts by itself

    panel.querySelector('#songs-play').click();
    expect(isScrolling()).toBe(true);
    expect(panel.querySelector('#songs-play').getAttribute('aria-label')).toBe('Pause');

    const sheet = panel.querySelector('#songs-sheet pre');
    sheet.click();
    expect(isScrolling()).toBe(false);
    sheet.click();
    expect(isScrolling()).toBe(true);

    panel.querySelector('#songs-faster').click();
    expect(panel.querySelector('#songs-speed').textContent).toBe('22');
  });

  it('scrolling by hand corrects the position but keeps it playing', async () => {
    const panel = await openSaved();
    autoscroll(panel).click();
    panel.querySelector('#songs-play').click();
    const sheet = panel.querySelector('#songs-sheet');
    sheet.dispatchEvent(new Event('wheel'));
    sheet.dispatchEvent(new Event('touchstart'));
    sheet.dispatchEvent(new Event('touchmove'));
    sheet.dispatchEvent(new Event('touchend'));
    expect(isScrolling()).toBe(true);
  });

  it('turning auto-scroll off mid-song stops it, and the switch is remembered', async () => {
    let panel = await openSaved();
    autoscroll(panel).click();
    panel.querySelector('#songs-play').click();
    expect(isScrolling()).toBe(true);

    autoscroll(panel).click();
    expect(isScrolling()).toBe(false);
    expect(panel.querySelector('#songs-scrollctl').hidden).toBe(true);

    autoscroll(panel).click();
    panel = await openSaved('s2');
    expect(autoscroll(panel).getAttribute('aria-pressed')).toBe('true');
    expect(panel.querySelector('#songs-scrollctl').hidden).toBe(false);
    expect(isScrolling()).toBe(false);
  });

  it('Space toggles auto-scroll only when it is switched on', async () => {
    const panel = await openSaved();
    document.dispatchEvent(new KeyboardEvent('keydown', { key: ' ' }));
    expect(isScrolling()).toBe(false);
    autoscroll(panel).click();
    panel.querySelector('#songs-sheet').dispatchEvent(new KeyboardEvent('keydown', { key: ' ', bubbles: true }));
    expect(isScrolling()).toBe(true);
    // Leaving the player stops it.
    panel.querySelector('#songs-back').click();
    expect(isScrolling()).toBe(false);
  });

  it('puts every control in the side rail and the title in the sheet', async () => {
    const panel = await openSaved();
    const rail = panel.querySelector('.songs-rail');
    for (const id of ['songs-back', 'songs-autoscroll', 'songs-tup', 'songs-fup', 'songs-listen-btn', 'songs-menu-btn', 'songs-full']) {
      expect(rail.querySelector(`#${id}`), id).not.toBeNull();
    }
    expect(panel.querySelector('#songs-sheet .songs-sheethead strong').textContent).toBe('Pompeii');
    // Panels open one at a time.
    panel.querySelector('#songs-menu-btn').click();
    expect(panel.querySelector('#songs-menu').hidden).toBe(false);
    panel.querySelector('#songs-listen-btn').click();
    expect(panel.querySelector('#songs-menu').hidden).toBe(true);
    expect(panel.querySelector('#songs-listen').hidden).toBe(false);
  });

  it('switches between versions from a search', async () => {
    const panel = await mount();
    panel.querySelector('#songs-go-find').click();
    panel.querySelector('#songs-q').value = 'pompeii';
    panel.querySelector('#songs-search-form').dispatchEvent(new Event('submit', { cancelable: true }));
    await settle();
    expect(panel.querySelector('.songs-result-more').textContent).toContain('2 versions');

    panel.querySelector('.songs-result-open').click();
    await settle();
    const select = panel.querySelector('#songs-version');
    expect(select.options.length).toBe(2);
    expect(panel.querySelector('#songs-sheet').textContent).toContain('first version');
    expect(panel.querySelector('#songs-prev').disabled).toBe(true);

    panel.querySelector('#songs-next').click();
    await settle();
    expect(panel.querySelector('#songs-sheet').textContent).toContain('second version');
    expect(panel.querySelector('#songs-version').value).toBe('1');
    expect(panel.querySelector('#songs-next').disabled).toBe(true);

    panel.querySelector('#songs-version').value = '0';
    panel.querySelector('#songs-version').dispatchEvent(new Event('change'));
    await settle();
    expect(panel.querySelector('#songs-sheet').textContent).toContain('first version');
  });

  it('a single version shows no switcher', async () => {
    const panel = await openSaved('s3');
    expect(panel.querySelector('#songs-version')).toBeNull();
    expect(panel.querySelector('#songs-prev')).toBeNull();
  });

  it('saves a fetched version without UG\'s version list', async () => {
    const panel = await mount();
    panel.querySelector('#songs-go-find').click();
    panel.querySelector('#songs-q').value = 'pompeii';
    panel.querySelector('#songs-search-form').dispatchEvent(new Event('submit', { cancelable: true }));
    await settle();
    panel.querySelector('.songs-result-open').click();
    await settle();
    panel.querySelector('#songs-save').click();
    await settle();
    const post = apiCalls.find(c => c.path === '/chords/songs' && c.opts.method === 'POST');
    expect(post.opts.body.tab_id).toBe(1001);
    expect(post.opts.body).not.toHaveProperty('versions');
  });

  it('embeds a pasted YouTube video by id and saves the link', async () => {
    const panel = await openSaved();
    panel.querySelector('#songs-listen-btn').click();
    expect(panel.querySelector('#songs-listen a').href).toContain('youtube.com/results?search_query=Pompeii');
    panel.querySelector('#songs-yt-url').value = 'https://www.youtube.com/watch?v=dQw4w9WgXcQ';
    panel.querySelector('#songs-yt-use').click();
    await settle();
    expect(panel.querySelector('.songs-video iframe').src).toBe('https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ?rel=0&modestbranding=1');
    const patch = apiCalls.find(c => c.opts.method === 'PATCH');
    expect(patch.opts.body).toEqual({ youtube_url: 'https://youtu.be/dQw4w9WgXcQ' });
  });

  it('asks the house Spotify player to play the song', async () => {
    const panel = await openSaved();
    panel.querySelector('#songs-listen-btn').click();
    panel.querySelector('#songs-sp-play').click();
    await settle();
    const call = apiCalls.find(c => c.path === '/chords/spotify');
    expect(call.opts.body).toEqual({ action: 'play', query: 'Pompeii Bastille' });
  });
});

describe('clampSpeed', () => {
  it('keeps speeds in range', () => {
    expect(clampSpeed(0)).toBe(2);
    expect(clampSpeed(999)).toBe(200);
    expect(clampSpeed('abc')).toBe(20);
  });
});
