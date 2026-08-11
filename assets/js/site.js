/*
 * 文書頁に2つだけ足す。頁内の目次と、見出しへの錨。
 *
 * どちらも無くても文書は読める。JavaScript を切っても内容が欠けない範囲に
 * 留め、雛形側で持てるものはここに置かない。
 */
(function () {
  "use strict";

  var source = document.querySelector("[data-toc-source]");
  if (!source) return;

  var headings = source.querySelectorAll("h2[id], h3[id]");

  /* 見出しへの錨。番号記号を押せば、その節の住所が得られる。 */
  source.querySelectorAll("h2[id], h3[id], h4[id]").forEach(function (h) {
    var a = document.createElement("a");
    a.className = "heading-anchor";
    a.href = "#" + h.id;
    a.textContent = "#";
    a.setAttribute("aria-label", "Link to this section");
    h.appendChild(a);
  });

  /* 頁内の目次。節が2つ以下の頁では帯を増やすだけなので出さない。 */
  var box = document.querySelector("[data-toc]");
  if (!box || headings.length < 3) return;

  var list = document.createElement("ol");
  headings.forEach(function (h) {
    var li = document.createElement("li");
    if (h.tagName === "H3") li.className = "toc__sub";
    var a = document.createElement("a");
    a.href = "#" + h.id;
    a.textContent = h.textContent.replace(/#$/, "");
    li.appendChild(a);
    list.appendChild(li);
  });

  var title = document.createElement("p");
  title.className = "toc__title";
  title.textContent = "On this page";

  box.appendChild(title);
  box.appendChild(list);
})();
