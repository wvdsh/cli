// Injected right after the SDK bundle tag, before any game script parses.
(function () {
	// Re-fetch the failed entry script to read its error body (e.g. the
	// resolver's "No entrypoint found for GODOT ..."), surfaced by the gate.
	window.__wavedashEntrypointError = function (src) {
		fetch(src, { cache: 'no-store' })
			.then(function (res) {
				return res.ok ? '' : res.text();
			})
			.catch(function () {
				return '';
			})
			.then(function (body) {
				window.__wavedashBootError =
					body.trim() || 'Failed to load entrypoint script: ' + src;
			});
	};

	// A failed boot refresh (expired session) surfaces as an unhandled
	// rejection; the server clears the cookie on 401, so a reload recovers.
	window.addEventListener('unhandledrejection', function (event) {
		if (String(event.reason).indexOf('Failed to refresh gameplay token') !== -1) {
			window.__wavedashBootError = 'Your dev session expired — reload the page to sign back in.';
		}
	});

	// Covers the page until the game calls Wavedash.init() — stricter than
	// prod's gate, deliberately. Translucent so engine loading UI shows through.
	function initGate() {
		if (window.Wavedash.initialized) return;
		var style = document.createElement('style');
		style.textContent = '@keyframes wd-spin{to{transform:rotate(360deg)}}';
		var spinner = document.createElement('div');
		spinner.style.cssText =
			'width:28px;height:28px;border-radius:50%;border:3px solid rgba(226,232,240,0.25);' +
			'border-top-color:#e2e8f0;animation:wd-spin 0.8s linear infinite';
		var msg = document.createElement('p');
		msg.style.cssText = 'margin:0;max-width:42ch;line-height:1.5';
		var overlay = document.createElement('div');
		overlay.style.cssText =
			'position:fixed;inset:0;z-index:2147483647;display:flex;flex-direction:column;' +
			'align-items:center;justify-content:center;gap:16px;background:rgba(8,10,18,0.65);' +
			'color:#e2e8f0;font:14px ui-sans-serif,system-ui,sans-serif;text-align:center';
		overlay.append(style, spinner, msg);
		document.body.append(overlay);
		var started = Date.now();
		var tick = setInterval(function () {
			if (window.Wavedash.initialized) {
				overlay.remove();
				clearInterval(tick);
				return;
			}
			// Re-attach if the game replaced the body's contents.
			if (!overlay.isConnected) document.body.append(overlay);
			if (window.__wavedashBootError) {
				msg.textContent = window.__wavedashBootError;
			} else if (Date.now() - started > 10000) {
				msg.textContent =
					"Your game hasn't called Wavedash.init() — on wavedash.com " +
					'the loading screen hangs exactly like this.';
			}
		}, 150);
	}

	// We run during <head> parsing — wait for a body to mount the overlay in.
	if (document.readyState === 'loading') {
		document.addEventListener('DOMContentLoaded', initGate);
	} else {
		initGate();
	}
})();
