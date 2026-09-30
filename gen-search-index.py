#!/usr/bin/env python3
"""Erzeugt search-index.json aus den Doku-Seiten, damit die Suche ueber alle
Seiten greift. Ohne diesen Schritt waere der Index nach der ersten Aenderung
veraltet.

    python3 gen-search-index.py
"""
import io, json, re, html

PAGES = ["docs.html", "getting-started.html", "connect.html", "concepts.html",
         "configuration.html", "guides.html", "api.html", "docker.html", "faq.html"]


def plain(s):
    return html.unescape(re.sub(r"\s+", " ", re.sub(r"<[^>]+>", " ", s))).strip()


index = []

for f in PAGES:
    s = io.open(f, encoding="utf-8").read()
    m = re.search(r'<article class="doc-main">(.*?)</article>', s, re.S) \
        or re.search(r'<div class="docs-hero">(.*?)</main>', s, re.S)
    body = m.group(1) if m else s

    # Hauptabschnitte
    for sec in re.finditer(r'<section class="doc-section" id="([^"]+)"[^>]*>(.*?)</section>', body, re.S):
        sid, content = sec.group(1), sec.group(2)
        h = re.search(r"<h([23])[^>]*>(.*?)</h\1>", content, re.S)
        title = plain(h.group(2)) if h else sid
        index.append({"page": f, "id": sid, "title": title, "text": plain(content)[:600]})

    # Werkzeug-Karten und FAQ-Eintraege einzeln, damit sie eigene Sprungziele bekommen
    for card in re.finditer(r'<div class="tool-card" id="([^"]+)">(.*?)</div>\s*</div>', s, re.S):
        sid, content = card.group(1), card.group(2)
        h = re.search(r"<h3>(.*?)</h3>", content, re.S)
        if h:
            index.append({"page": f, "id": sid, "title": plain(h.group(1)), "text": plain(content)[:600]})

    for item in re.finditer(r'<div class="faq-item" id="([^"]+)">\s*<h3>(.*?)</h3>(.*?)</div>', s, re.S):
        index.append({"page": f, "id": item.group(1), "title": plain(item.group(2)),
                      "text": plain(item.group(3))[:600]})

seen, uniq = set(), []
for e in index:
    k = (e["page"], e["id"])
    if k not in seen:
        seen.add(k)
        uniq.append(e)

io.open("search-index.json", "w", encoding="utf-8").write(
    json.dumps(uniq, ensure_ascii=False, indent=1))
print("search-index.json: %d Eintraege" % len(uniq))
