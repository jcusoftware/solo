const list = document.querySelector('#event-list');
const status = document.querySelector('#event-status');

function element(tag, text) {
  const node = document.createElement(tag);
  node.textContent = text;
  return node;
}

async function loadEvents() {
  const response = await fetch('/api/events');
  if (!response.ok) throw new Error('Events unavailable');
  const { events } = await response.json();
  const formatter = new Intl.DateTimeFormat(document.documentElement.lang, {
    dateStyle: 'full', timeStyle: 'short', timeZone: list.dataset.timezone,
  });
  const cards = document.createDocumentFragment();
  const graph = [];
  for (const event of events) {
    const card = element('li', '');
    card.className = 'card';
    const when = element('time', formatter.format(new Date(event.start_utc)));
    when.dateTime = event.start_utc;
    card.append(element('h2', event.title), when);
    if (event.end_utc) {
      const end = element('time', formatter.format(new Date(event.end_utc)));
      end.dateTime = event.end_utc;
      card.append(' – ', end);
    }
    if (event.location) card.append(element('p', event.location));
    if (event.description) card.append(element('p', event.description));
    const calendar = element('a', `Download calendar entry for ${event.title} (.ics)`);
    calendar.href = `/api/events/${encodeURIComponent(event.id)}/ics`;
    const link = element('p', '');
    link.append(calendar);
    card.append(link);
    cards.append(card);
    graph.push({
      '@type': 'Event', name: event.title, startDate: event.start_utc,
      endDate: event.end_utc ?? undefined, description: event.description ?? undefined,
      location: event.location ? { '@type': 'Place', name: event.location } : undefined,
      isAccessibleForFree: true,
    });
  }
  list.replaceChildren(cards);
  if (graph.length) {
    const data = element('script', JSON.stringify({ '@context': 'https://schema.org', '@graph': graph }));
    data.type = 'application/ld+json';
    document.head.append(data);
  }
  status.textContent = events.length ? `${events.length} upcoming free events.` : 'No upcoming free events are listed.';
}

status.textContent = 'Loading events…';
loadEvents().catch(() => { status.textContent = 'Events could not be loaded. Please try again later.'; });
