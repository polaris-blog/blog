// Entry Gate — Cloudflare-style proof-of-work human verification.
//
// Flow:
//   1. `request_guard` intercepts content pages while the visitor holds no
//      valid clearance cookie and redirects to /plugins/gate/verify.
//   2. verify issues a one-time challenge (plugin-cache, replay protected)
//      and serves a self-contained page whose browser solves a small
//      proof-of-work (sha256 leading zeros via Web Crypto).
//   3. check validates the solution server-side, consumes the challenge,
//      signs a clearance token with sign_hex (plugin-scoped HMAC) and hands
//      it to the browser as a cookie.
//
// Clearance tokens survive server restarts: the signing key is derived from
// the instance secret (sign_hex), never from volatile state.
//
// Configuration (config.toml, all optional):
//   difficulty      hex leading zeros the proof must match (default "0000",
//                   roughly 65k hashes — under a second on a phone)
//   cookie_hours    clearance lifetime in hours (default 24)
//   challenge_ttl   seconds a challenge stays valid (default 300)

const COOKIE = "gate_clearance";
const SAFE_PATH_CHARS = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~/?&=%";

fn cfg(key, fallback) {
    let v = config_get(key);
    if v == "" { fallback } else { v }
}

fn difficulty() { cfg("difficulty", "0000") }

fn query_param(q, key) {
    if q == "" { return ""; }
    for part in q.split("&") {
        let eq = part.index_of("=");
        if eq > 0 {
            if sub_string(part, 0, eq) == key {
                return sub_string(part, eq + 1, part.len - eq - 1);
            }
        }
    }
    return "";
}

fn safe_path(p) {
    if sub_string(p, 0, 1) != "/" { return false; }
    if sub_string(p, 0, 2) == "//" { return false; }
    for ch in p.split("") {
        if ch != "" && SAFE_PATH_CHARS.index_of(ch) < 0 { return false; }
    }
    return true;
}

// -- request guard hook (permission: request.guard) --------------------------

fn request_guard(req) {
    // Signed-in visitors (admins, comment authors) are never challenged.
    if req.has_session { return (); }
    // The guard only arbitrates content pages; it never sees /plugins itself,
    // and non-GET requests (form posts) are not gated.
    if req.method != "GET" { return (); }

    let c = if req.cookies.contains(COOKIE) { req.cookies[COOKIE] } else { "" };
    if valid_clearance(c) { return (); }

    let target = req.path + (req.query == "" ? "" : "?" + req.query);
    return #{redirect: `/plugins/gate/verify?back=${base64_encode(target)}`};
}

fn valid_clearance(c) {
    let dot = c.index_of(".");
    if dot < 0 { return false; }
    let expiry = parse_int(sub_string(c, 0, dot));
    let mac = sub_string(c, dot + 1, c.len - dot - 1);
    if now() > expiry { return false; }
    return sign_hex(`${expiry}`) == mac;
}

// -- verify: issue a challenge and serve the PoW page ------------------------

fn route_verify(req) {
    let ttl = parse_int(cfg("challenge_ttl", "300"));
    let seq = parse_int(cache_get("seq"));
    let next = if seq <= 0 { 1 } else { seq + 1 };
    cache_set("seq", `${next}`, 3600);
    let challenge = sha256_hex(`gate:${now()}:${next}:${req.ip}`);
    cache_set(`ch:${challenge}`, "1", ttl);
    let back64 = query_param(req.query, "back");
    let html = pow_page(challenge, difficulty(), back64);
    #{status: 200, content_type: "text/html; charset=utf-8", body: html}
}

// -- check: validate the proof and hand out the clearance cookie -------------

fn route_check(req) {
    let cookie_hours = parse_int(cfg("cookie_hours", "24"));
    let difficulty = difficulty();
    let challenge = query_param(req.query, "c");
    let n = query_param(req.query, "n");
    let back64 = query_param(req.query, "back");
    let back = base64_decode(back64);

    // Replay protection: every challenge is consumed exactly once.
    if cache_get(`ch:${challenge}`) == "" {
        return reject("Unknown or expired challenge. Please try again.");
    }
    cache_del(`ch:${challenge}`);

    // The proof must match the announced difficulty.
    if !sha256_hex(`${challenge}:${n}`).starts_with(difficulty) {
        return reject("Invalid proof of work. Please try again.");
    }

    // Open-redirect guard: only in-site paths survive.
    if !safe_path(back) {
        return reject("Invalid redirect target.");
    }

    let expiry = now() + cookie_hours * 3600;
    let token = `${expiry}.${sign_hex(`${expiry}`)}`;
    let html = `<script>document.cookie="${COOKIE}=${token}; Path=/; Max-Age=${cookie_hours * 3600}; SameSite=Lax";location.href="${back}";</script>`;
    #{status: 200, content_type: "text/html; charset=utf-8", body: html}
}

fn reject(msg) {
    `<script>setTimeout(function(){location.href="/plugins/gate/verify";},1500);</script>` +
    `<p style="font-family:system-ui,sans-serif;padding:2rem">${msg}</p>`
}

// -- self-contained proof-of-work page ---------------------------------------
//
// The browser hashes "challenge:nonce" with Web Crypto (SHA-256) until the
// hex digest starts with the configured zeros. Requires HTTPS (Web Crypto is
// unavailable on insecure origins) — direct-HTTP visitors see a notice.

fn pow_page(challenge, difficulty, back64) {
    `<style>body{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;background:#000;color:#f5f5f7;font-family:system-ui,-apple-system,sans-serif}main{text-align:center;padding:1rem}h1{font-size:22px;font-weight:600;margin:0 0 12px}p{color:rgba(245,245,247,.6);margin:0;font-size:14px;max-width:420px}</style>
<main><h1>Verifying you are human…</h1><p id="s">This page is checking your browser.</p></main>
<script>
(async function () {
    var c = "${challenge}";
    var d = "${difficulty}";
    var back = "${back64}";
    var s = document.getElementById("s");
    if (!window.crypto || !crypto.subtle) {
        s.textContent = "This check needs HTTPS — please visit via the site's public URL.";
        return;
    }
    var enc = new TextEncoder();
    var n = 0, t0 = Date.now();
    s.textContent = "Solving a proof-of-work challenge…";
    while (true) {
        var digest = new Uint8Array(await crypto.subtle.digest("SHA-256", enc.encode(c + ":" + n)));
        var hex = Array.prototype.map.call(digest, function (b) { return ("0" + b.toString(16)).slice(-2); }).join("");
        if (hex.slice(0, d.length) === d) {
            location.href = "/plugins/gate/check?c=" + encodeURIComponent(c) + "&n=" + n + "&back=" + encodeURIComponent(back);
            return;
        }
        n++;
        if (n % 500 === 0) {
            s.textContent = "Solving a proof-of-work challenge… " + n + " hashes";
            await new Promise(function (r) { setTimeout(r, 0); });
        }
        if (Date.now() - t0 > 20000) {
            s.textContent = "Taking longer than expected — still working…";
        }
    }
})();
</script>`
}
