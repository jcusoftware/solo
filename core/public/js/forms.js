// Progressive-enhancement island for forms posting to /api/* (tenant-agnostic).
// Without JS the form performs a normal POST (Phase 0: the API answers JSON).
// With JS: submits via fetch, announces results in the form's role="status"
// region, marks invalid fields with aria-invalid and moves focus to the first.
(function () {
  'use strict';

  var MESSAGES = {
    validation_failed: 'Please check the highlighted fields and try again.',
    turnstile_required: 'Please complete the security check, then submit again.',
    turnstile_failed: 'The security check failed. Please try again.',
    turnstile_unavailable: 'The security check is temporarily unavailable. Please try again shortly.',
    turnstile_unconfigured: 'This form is temporarily unavailable. Please try again later.',
    payload_too_large: 'Your message is too long. Please shorten it and try again.',
    storage_failed: 'Sorry, something went wrong on our side. Please try again later.',
    network: 'We could not reach the server. Check your connection and try again.'
  };
  var FIELD_MESSAGES = {
    required: 'is required',
    invalid: 'is not valid',
    too_long: 'is too long',
    invalid_characters: 'contains characters we cannot accept'
  };

  function cssEscape(value) {
    if (window.CSS && typeof window.CSS.escape === 'function') {
      return window.CSS.escape(value);
    }
    return String(value).replace(/([^\w-])/g, '\\$1');
  }

  function isHiddenOrInert(el) {
    if (!el || el.type === 'hidden') return true;
    return Boolean(el.closest && el.closest('[aria-hidden="true"]'));
  }

  function resolveField(form, name) {
    var el = form.elements && form.elements[name];
    if (!el) return null;
    if (typeof el.tagName !== 'string' && typeof el.length === 'number') {
      for (var i = 0; i < el.length; i++) {
        if (!isHiddenOrInert(el[i])) return el[i];
      }
      return null;
    }
    return isHiddenOrInert(el) ? null : el;
  }

  function labelFor(form, name) {
    var input = resolveField(form, name);
    if (!input) return name;
    var label = input.id ? form.querySelector('label[for="' + cssEscape(input.id) + '"]') : null;
    if (!label && input.closest) label = input.closest('label');
    var text = label ? label.textContent.trim() : name;
    return text.length > 60 ? text.slice(0, 57) + '…' : text;
  }

  function setFieldError(form, input, message) {
    if (!input.hasAttribute('data-orig-describedby')) {
      input.setAttribute('data-orig-describedby', input.getAttribute('aria-describedby') || '');
    }
    var errId = (input.id || input.name || 'field') + '-error';
    var errEl = form.querySelector('#' + cssEscape(errId));
    if (!errEl) {
      errEl = document.createElement('p');
      errEl.className = 'field-error';
      errEl.id = errId;
      var wrap = input.closest ? input.closest('.field') : null;
      if (wrap) {
        wrap.appendChild(errEl);
      } else if (input.parentNode) {
        input.parentNode.insertBefore(errEl, input.nextSibling);
      }
    }
    errEl.textContent = message;
    var orig = input.getAttribute('data-orig-describedby') || '';
    var tokens = orig ? orig.trim().split(/\s+/) : [];
    if (tokens.indexOf(errId) === -1) tokens.push(errId);
    input.setAttribute('aria-describedby', tokens.join(' '));
  }

  function clearInvalid(form) {
    var marked = form.querySelectorAll('[aria-invalid="true"]');
    for (var i = 0; i < marked.length; i++) {
      var el = marked[i];
      el.removeAttribute('aria-invalid');
      if (el.hasAttribute('data-orig-describedby')) {
        var orig = el.getAttribute('data-orig-describedby');
        if (orig) {
          el.setAttribute('aria-describedby', orig);
        } else {
          el.removeAttribute('aria-describedby');
        }
        el.removeAttribute('data-orig-describedby');
      }
    }
    var errs = form.querySelectorAll('.field-error');
    for (var j = 0; j < errs.length; j++) {
      if (errs[j].parentNode) errs[j].parentNode.removeChild(errs[j]);
    }
  }

  function resetTurnstile(form) {
    var widget = form.querySelector('.cf-turnstile');
    if (widget && window.turnstile && typeof window.turnstile.reset === 'function') {
      try { window.turnstile.reset(widget); } catch (e) { /* widget not rendered yet */ }
    }
  }

  function enhance(form) {
    var status = form.querySelector('[role="status"]');
    var button = form.querySelector('button[type="submit"], button:not([type]), input[type="submit"]');
    var inFlight = false;

    function say(text) {
      if (status) status.textContent = text;
    }

    form.addEventListener('submit', function (event) {
      if (!window.fetch || !window.URLSearchParams || !window.FormData) return;
      event.preventDefault();
      if (inFlight) return;
      inFlight = true;
      clearInvalid(form);
      say('Sending…');
      form.setAttribute('aria-busy', 'true');
      if (button) button.disabled = true;

      var action = form.getAttribute('action') || '';
      var body = new URLSearchParams(new FormData(form));
      var controller = typeof window.AbortController === 'function' ? new window.AbortController() : null;
      var timer = controller ? window.setTimeout(function () { controller.abort(); }, 15000) : null;
      var shouldResetTurnstile = true;

      var fetchOpts = {
        method: 'POST',
        body: body,
        headers: { Accept: 'application/json' },
        credentials: 'same-origin'
      };
      if (controller) fetchOpts.signal = controller.signal;

      fetch(action, fetchOpts)
        .then(function (res) {
          return res.json().catch(function () { return {}; }).then(function (data) {
            return { ok: res.ok, data: data || {} };
          });
        })
        .then(function (result) {
          if (result.ok && result.data.ok) {
            form.reset();
            say(form.getAttribute('data-success') || 'Thank you. Your submission was received.');
            return;
          }
          var data = result.data;
          if (data.error === 'turnstile_required' || data.error === 'validation_failed') {
            shouldResetTurnstile = false;
          }
          var parts = [];
          var first = null;
          if (data.fields) {
            Object.keys(data.fields).forEach(function (name) {
              var input = resolveField(form, name);
              var msg = labelFor(form, name) + ' ' + (FIELD_MESSAGES[data.fields[name]] || 'needs attention') + '.';
              if (input) {
                input.setAttribute('aria-invalid', 'true');
                setFieldError(form, input, msg);
                if (!first) first = input;
              }
              parts.push(msg);
            });
          }
          say('Error: ' + (MESSAGES[data.error] || 'Your submission could not be sent.') + (parts.length ? ' ' + parts.join(' ') : ''));
          if (first) first.focus();
        })
        .catch(function () {
          say('Error: ' + MESSAGES.network);
        })
        .then(function () {
          if (timer !== null) window.clearTimeout(timer);
          inFlight = false;
          form.removeAttribute('aria-busy');
          if (button) button.disabled = false;
          if (shouldResetTurnstile) resetTurnstile(form);
        });
    });
  }

  var forms = document.querySelectorAll('form.js-api-form');
  for (var i = 0; i < forms.length; i++) enhance(forms[i]);
})();
