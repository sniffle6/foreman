/* Reusable retrieval-practice quiz component.
   Usage in a lesson:
     <div class="quiz" id="quiz1"></div>
     <script src="../assets/quiz.js"></script>
     <script>
       renderQuiz(document.getElementById("quiz1"), [
         { prompt: "…", options: ["a","b","c","d"], answer: 0, expl: "…" },
       ]);
     </script>
   Rules for authors: options must have equal word counts (no length tells);
   `answer` is the index into `options` BEFORE shuffling (this component
   shuffles display order itself). Feedback is immediate; explanation shows
   after the first click; one attempt per question. */

function renderQuiz(root, questions) {
  let answered = 0;
  let correct = 0;

  const scoreEl = document.createElement("p");
  scoreEl.className = "score";
  scoreEl.textContent = "0 / " + questions.length + " answered";

  questions.forEach(function (q, qi) {
    const box = document.createElement("div");
    box.className = "q";

    const prompt = document.createElement("p");
    prompt.className = "prompt";
    prompt.textContent = (qi + 1) + ". " + q.prompt;
    box.appendChild(prompt);

    const opts = document.createElement("div");
    opts.className = "opts";

    // shuffle display order, remember where the right answer went
    const order = q.options.map(function (_, i) { return i; });
    for (let i = order.length - 1; i > 0; i--) {
      const j = Math.floor(Math.random() * (i + 1));
      const t = order[i]; order[i] = order[j]; order[j] = t;
    }

    order.forEach(function (origIdx) {
      const b = document.createElement("button");
      b.className = "opt";
      b.type = "button";
      b.textContent = q.options[origIdx];
      b.addEventListener("click", function () {
        if (box.classList.contains("answered")) return;
        box.classList.add("answered");
        answered++;
        const isRight = origIdx === q.answer;
        if (isRight) correct++;
        b.classList.add(isRight ? "right" : "wrong");
        // always reveal the correct option
        Array.prototype.forEach.call(opts.children, function (btn, k) {
          btn.disabled = true;
          if (order[k] === q.answer) btn.classList.add("right");
        });
        scoreEl.textContent =
          correct + " / " + answered + " correct" +
          (answered === questions.length ? " — done" : "");
      });
      opts.appendChild(b);
    });
    box.appendChild(opts);

    const expl = document.createElement("div");
    expl.className = "expl";
    expl.innerHTML = q.expl || "";
    box.appendChild(expl);

    root.appendChild(box);
  });

  root.appendChild(scoreEl);
}
