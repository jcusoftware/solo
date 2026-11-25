const byId = (id) => document.getElementById(`booking-${id}`);
const form = byId('form');
const date = byId('date');
const status = byId('status');
const find = byId('find');
const times = byId('times');
const options = byId('options');
const submit = byId('submit');
const summary = byId('errors');
const timezone = form.dataset.timezone;
const formatTime = new Intl.DateTimeFormat(document.documentElement.lang, {
  hour: 'numeric', minute: '2-digit', timeZoneName: 'short', timeZone: timezone,
});
const formatDate = new Intl.DateTimeFormat(document.documentElement.lang, {
  dateStyle: 'full', timeZone: timezone,
});

const parts = Object.fromEntries(new Intl.DateTimeFormat('en-US', {
  year: 'numeric', month: '2-digit', day: '2-digit', timeZone: timezone,
}).formatToParts(new Date()).map(({ type, value }) => [type, value]));
date.min = `${parts.year}-${parts.month}-${parts.day}`;
const lastDay = new Date(`${date.min}T00:00:00Z`);
lastDay.setUTCDate(lastDay.getUTCDate() + 59);
date.max = lastDay.toISOString().slice(0, 10);
form.noValidate = true;
form.hidden = false;

let loading;
let submitting = false;

function inlineError(field, message) {
  const error = byId(`${field}-error`);
  error.textContent = message;
  error.hidden = !message;
  const controls = field === 'time' ? options.querySelectorAll('input') : [byId(field)];
  for (const control of controls) {
    if (message) control.setAttribute('aria-invalid', 'true');
    else control.removeAttribute('aria-invalid');
  }
}

function clearErrors() {
  for (const field of ['date', 'time', 'name', 'email']) inlineError(field, '');
  summary.hidden = true;
}

function showErrors(errors) {
  const list = summary.querySelector('ul');
  list.replaceChildren(...errors.map(([field, message]) => {
    if (field) inlineError(field, message);
    const item = document.createElement('li');
    if (!field) item.textContent = message;
    else {
      const control = field === 'time' ? options.querySelector('input') : byId(field);
      const link = document.createElement('a');
      link.href = `#${control.id}`;
      link.textContent = message;
      link.addEventListener('click', () => control.focus());
      item.append(link);
    }
    return item;
  }));
  summary.hidden = false;
  summary.focus();
}

function clearTimes() {
  loading?.abort();
  options.replaceChildren();
  times.hidden = true;
  submit.disabled = true;
  status.textContent = '';
}

async function loadTimes({ preserveError = false } = {}) {
  clearTimes();
  if (!preserveError) clearErrors();
  if (!date.validity.valid) {
    showErrors([['date', 'Enter a date within the next 60 days.']]);
    return;
  }
  const request = new AbortController();
  loading = request;
  status.textContent = 'Loading available times…';
  try {
    const response = await fetch(`/api/bookings/slots?date=${encodeURIComponent(date.value)}`, { signal: request.signal });
    if (!response.ok) throw new Error('Times unavailable');
    const { slots } = await response.json();
    if (request.signal.aborted) return;
    const radios = slots.map(({ start_utc }, index) => {
      const radio = document.createElement('input');
      radio.type = 'radio';
      radio.id = `booking-slot-${index}`;
      radio.name = 'start_utc';
      radio.value = start_utc;
      radio.required = true;
      radio.setAttribute('aria-describedby', 'booking-time-error');
      const label = document.createElement('label');
      label.append(radio, formatTime.format(new Date(start_utc)));
      return label;
    });
    options.replaceChildren(...radios);
    times.hidden = !radios.length;
    submit.disabled = !radios.length;
    status.textContent = radios.length ? 'Available times loaded. Choose a time below.' : 'No times available. Choose another date.';
  } catch {
    if (!request.signal.aborted) status.textContent = 'Times could not be loaded. Try again or choose another date.';
  }
}

date.addEventListener('change', () => { clearErrors(); clearTimes(); });
find.addEventListener('click', () => loadTimes());

form.addEventListener('submit', async (event) => {
  event.preventDefault();
  if (submitting || submit.disabled) return;
  clearErrors();
  const body = new URLSearchParams(new FormData(form));
  const errors = [];
  if (!date.validity.valid) errors.push(['date', 'Enter a date within the next 60 days.']);
  if (!body.get('start_utc')) errors.push(['time', 'Choose an available time.']);
  if (!body.get('guest_name')?.trim()) errors.push(['name', 'Enter your name.']);
  if (!byId('email').validity.valid) errors.push(['email', 'Enter a valid email address.']);
  if (!errors.length && !body.get('cf-turnstile-response')) errors.push([null, 'Complete the security check and try again.']);
  if (errors.length) { showErrors(errors); return; }

  submitting = true;
  for (const control of [date, find, times, submit]) control.disabled = true;
  status.textContent = 'Submitting your booking…';
  const stop = (message) => { form.hidden = true; showErrors([[null, message]]); };
  try {
    const response = await fetch(form.action, { method: 'POST', body });
    const result = await response.json();
    if (response.status === 201 && result.status === 'confirmed') {
      const confirmation = byId('confirmation');
      const time = confirmation.querySelector('time');
      const start = new Date(result.start_utc);
      time.dateTime = result.start_utc;
      time.textContent = `${formatDate.format(start)} at ${formatTime.format(start)}`;
      form.hidden = true;
      confirmation.hidden = false;
      confirmation.focus();
    } else if (result.error === 'booking_pending_review') {
      stop(`We cannot confirm your booking yet. Do not submit again. Contact the venue with reference ${result.reference}.`);
    } else if (response.status >= 500) {
      stop('We could not confirm whether your booking went through. Contact the venue before trying again.');
    } else if (response.status === 409) {
      showErrors([[null, 'That time is no longer available. Choose a different time.']]);
      await loadTimes({ preserveError: true });
    } else {
      showErrors([[null, result.error?.startsWith('turnstile_')
        ? 'Complete a new security check and try again.' : 'Check your booking details and try again.']]);
    }
  } catch {
    stop('We could not confirm whether your booking went through. Contact the venue before trying again.');
  } finally {
    submitting = false;
    if (!form.hidden) {
      for (const control of [date, find, times]) control.disabled = false;
      submit.disabled = !options.children.length;
      if (status.textContent === 'Submitting your booking…') status.textContent = '';
      window.turnstile?.reset();
    }
  }
});
