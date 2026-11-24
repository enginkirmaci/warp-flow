# warp-flow akış motoru — inceleme ve optimizasyon raporu

**Tarih:** 24 Kasım 2026
**Kapsam:** `crates/warp-flow-core` (motor), `src/host.rs`, `src/store.rs`,
`src/ui/{actions,run,runs,window,chrome}.rs`, `.flow/workflow.toml`, ilgili
ADR'ler ve `AGENTS.md`.
**Yöntem:** statik okuma, akış izleme, mevcut testler ve clippy.
**Kod değişikliği:** yok. Bu belge yalnızca analizdir.

Doğrulama:

- `cargo test --locked -p warp-flow-core` → 200 test geçti.
- `cargo test --locked` (app probe'ları) → geçti.
- `cargo clippy --locked -p warp-flow-core --all-targets -- -D warnings` → temiz.

> Not: Satır numaraları inceleme anındaki koda göredir; inceleme sırasında
> ağaçta bulunan UI değişiklikleri `5bf5a54` olarak commit edildi. Bu rapor
> kodu değiştirmez.

---

## 0. Yönetici özeti

Mutlu yol (analyze → onay → kod → inceleme → kapılar → bump → merge →
summary/cleanup) büyük ölçüde doğru kurgulanmış. Paralel iş için worktree
ayrımı, "bir adım bir klasör" kuralı ve merge bariyeri sağlam. Testler de bu
davranışların çoğunu koruyor.

Buna karşılık, üretimde en çok acıtacak üç **P0** var:

1. **Onay sorusuna "Hayır" demek, `answer_no` kenarı tanımlı değilse koşuyu
   ilerletiyor.** Varsayılan `.flow/workflow.toml` içindeki `approve-plan`
   sorusunun kenarı yok; yani "No" tıklamak planı onaylamış gibi `code`
   adımını başlatıyor. Bu, insan onayı kapısını tamamen bypass ediyor.
2. **Koşu sonunda issue klasörü commit edilmiyor; merge'in temizlik kontrolü
   ise tüm çalışma ağacını görüyor.** Sonuç: `summary.md` ve nihai `run.md`
   `main`'e hiç girmiyor; bir sonraki koşunun merge'ü, önceki koşunun
   bıraktığı kirli `.scratch/` yüzünden `DirtyMain` ile bloke oluyor. Aynı
   şey koşu sırasında kaydedilen `.flow/workflow.toml` ve "New run" ile açılan
   yeni klasör için de geçerli.
3. **Aynı issue/repo üzerinde ikinci bir motoru engelleyen kalıcı kilit yok.**
   Host yalnızca kendi süreci içindeki koşuları görüyor. CLI aynı koşuyu
   sürerken arayüz "Resume (takeover)" sunuyor ve ikinci bir motor başlatıyor;
   iki motor aynı `run.md`, `questions/` ve merge kuyruğuna yazabiliyor.

Bunların dışında:

- **Retry mekanizması üç yerde yarım:** edge bütçesi insan retry'ında
  sıfırlanmıyor; boş branch'ten sonra retry aynı sonucu veriyor; merge
  sonrası (rebase sonrası) kapı hatasının `code`'a dönüş yolu yok.
- **Kapı hatası bir edge'e dönüştüğünde hata çıktısı kayboluyor.** `test →
  code` döngüsü, hangi testin kırıldığını söylemeden yeniden başlıyor.
- **Bildirim yüzeyi pasif:** yalnızca rail'deki sayı ve liste satırı var;
  koşu "waiting"/"blocked" olduğunda masaüstü bildirimi, başlık çubuğu
  aktivitesi veya geçici bir kart yok.
- **Modelin akışı şekillendirmesi** bugün sadece `verdict` ve görev
  dosyalarındaki `Blocked by:` üzerinden mümkün. Dinamik adım ekleme,
  oturum içi soru sorma ve adım bazlı retry/paralellik politikası yok;
  plan dokümanında (`docs/plans/warp-flow.md:230`) bahsedilen `research` /
  `repair` düğümleri henüz uygulanmamış.

---

## 1. P0 bulgular

### P0-1 — "Hayır" cevabı, kenar yoksa onay sayılıyor

**Kanıt**

- `crates/warp-flow-core/src/run.rs:1448-1477`: Soru adımı yanıtlandığında
  `answer_is_yes` ile `AnswerYes`/`AnswerNo` tetiği seçiliyor; **yalnızca o
  tetiğe uyan bir edge varsa** `take_edge` çağrılıyor. Edge yoksa satır
  `done` olarak yazılıp `StepRun::Done` dönülüyor.
- `crates/warp-flow-core/src/run.rs:1836-1841`: "yes" dışındaki her şey "no"
  sayılıyor.
- `.flow/workflow.toml:63-64`: `approve-plan` sadece `kind = "question"`;
  `edges` yok.
- `src/ui/run.rs:472-497`: onay sorusunda "Yes" ve "No" düğmeleri her zaman
  çiziliyor; "No" doğrudan `submit_answer("no")` çağırıyor.

**Etki**

İnsan "No" diyerek planı reddettiğinde motor bunu "adım tamamlandı" olarak
yorumluyor ve `code` adımına geçiyor. Bu, ürünün en temel güvenlik kapısını
(insan onayı) etkisiz bırakır. Ayrıca serbest metin cevap `picked` değerini
ezdiği için (`src/ui/run.rs:123-153`), "planı beğenmedim" gibi bir metin de
"no" olur; edge yoksa yine onay sayılır.

**Öneri**

- Edge yoksa ve cevap "yes" değilse adımı **asla `done` yazma**: `Blocked`
  (gerekçe: "plan reddedildi, hangi adıma dönüleceği tanımlı değil") veya
  soruyu açık bırakıp yeni bir tur bekle.
- Alternatif/ek: `Workflow::validate`, etkin bir `question` adımı için
  `answer_no` (veya `answer_yes`) edge'i zorunlu tutsun; en azından
  `answer_no` yoksa "no"nun ne yapacağını grafik yazarı açıkça belirtmek
  zorunda kalsın.
- Test: "no answer with no edge blocks / does not advance".

Bu tek bulgu, mevcut varsayılan şablonu kullanan her koşuyu etkiler.

---

### P0-2 — Kapanış commit'i yok + merge temizlik kontrolü fazla geniş

**Kanıt**

- `crates/warp-flow-core/src/run.rs:1125-1166` (`start`): `run.md` yazılıyor,
  commit yok. AGENTS'in söz verdiği `flow: issue <slug>` commit'i kodda hiç
  yok.
- `crates/warp-flow-core/src/run.rs:1885` ve `:1899`: yalnızca merge öncesi
  `flow: record <slug>` ve görev çözüldükten sonra `flow: resolve task NN`
  var.
- `crates/warp-flow-core/src/run.rs:699-707`: `Done` durumunda sadece
  `write_run()` var; commit yok. `summary` ve `cleanup` adımları merge'den
  sonra çalıştığı için `summary.md` ve nihai `run.md` **hiç commit edilmiyor**.
- `crates/warp-flow-core/src/run.rs:2078-2135` (`commit_issue`): yalnızca
  **kendi issue klasörünü** stage ediyor.
- `crates/warp-flow-core/src/merge.rs:111-123`: merge, `git status
  --porcelain` çıktısı boş değilse `DirtyMain` dönüyor; yani tüm ağaç.
- `crates/warp-flow-core/src/prepare.rs:222-229,297-302`: koşu başlatma
  kontrolü `.scratch/` altını "motorun alanı" sayıp yok sayıyor.
- `src/ui/new_run.rs:147`: yeni koşu yalnızca `.scratch/<slug>/spec.md`
  yazıyor; commit yok.
- `src/ui/screens/workflow.rs:839-875`: şablon kaydı yalnızca dosyayı yazıyor;
  commit yok.

**Etki (zincirleme)**

1. Koşu A biter: `summary.md` + nihai `run.md` çalışma ağacında kirli kalır.
2. Koşu B başlar: `require_clean` `.scratch/`'ı yok saydığı için başlar.
3. Koşu B merge öncesi yalnızca kendi klasörünü commit eder; `merge()` tüm
   ağaca bakar ve A'nın artıklarını görür → `DirtyMain`.
4. `DirtyMain` `Retry::Never` ile bloke eder (`run.rs:1998-2035`); retry aynı
   sonucu verir. Kişi elle commit edene kadar koşu ilerleyemez.

Aynı mekanizma, koşu sırasında `.flow/workflow.toml` kaydedildiğinde (Repos ›
Workflow bunu açıkça teşvik ediyor) veya "New run" ile yeni bir klasör
açıldığında da merge'ü bloke eder. Yani motorun kendi ürettiği meşru yazılar,
motorun merge'ünü durduruyor.

**Öneri**

- `flow: issue <slug>` commit'ini `start()`'a ekle.
- `Done`/`Blocked`/`Abandoned` gibi terminal durumlarda **kapanış commit'i**
  yap (yalnızca issue klasörü, `main` üzerindeyken, hooks/signing kapalı).
- `commit_issue`'yu motorun sahip olduğu tüm alanı (en az `.scratch/`) veya
  en azından "bu koşunun klasörü + bekleyen motor yazıları"nı kapsayacak
  şekilde genişlet; alternatif olarak merge'in temizlik kontrolünü
  `git status --porcelain -- . ':(exclude).scratch' ':(exclude).flow'`
  benzeri şekilde daralt. İkincisi, "dirty working tree refusal" kuralının
  ruhunu korurken motorun kendi yazılarını dışarıda bırakır.
- Test: koşu bittikten sonra `git status` temiz; ikinci koşu merge'e
  ulaşabiliyor; koşu sırasında `.flow` kaydı merge'ü bloke etmiyor.

Not: `AGENTS.md` "üç noktada commit" diyor; uygulamada iki nokta var, biri de
kapanışı kapsamıyor. Doküman ile kod burada ayrışmış.

---

### P0-3 — Süreçler arası koşu kilidi yok

**Kanıt**

- `src/host.rs:566-577`: "bir repoda bir koşu" kontrolü yalnızca
  `self.runs` (bu sürecin belleği) üzerinden.
- `crates/warp-flow-core/src/run.rs` ve `src/host.rs` içinde `flock`,
  kilit dosyası veya pid kontrolü yok.
- `src/ui/actions.rs:228-235`: kaydı `running` olan ama bu pencerenin
  sürmediği koşu için **Resume** sunuluyor; açıklama "Not driven by this
  window; Resume takes it over."
- `src/ui/screens/mod.rs:204-210`: artifact düzenleme izni de yalnızca yerel
  host'un fazına bakıyor.

**Etki**

- CLI (veya ikinci bir uygulama örneği) aynı issue'yu sürerken arayüzden
  Resume'a basmak ikinci bir `Runner` başlatır. İki motor aynı `run.md`,
  `questions/NN.md`, `repairs/NN.md` ve merge kuyruğuna yazar; numaralandırma
  ve merge yarışı bozulur.
- Aynı şekilde CLI sürerken Artifacts sekmesinden task/`run.md` düzenlemek
  mümkün; "motor sürerken klasör motorun" kuralı süreç dışına taşmıyor.

**Öneri**

- `.scratch/<slug>/.warp-flow.lock` üzerinde `flock` (veya `fs2`/`fd-lock`)
  ile koşu sahipliği; `Runner::run` boyunca tut, süreç ölürse çekirdek
  bırakır.
- Kilit sahibini (pid, başlangıç zamanı, uygulama sürümü) kilide yaz; UI
  "başka bir süreç sürüyor" gösterip Resume'u **açık bir "devral" onayı**
  olmadan sunmasın.
- Uygulama örneği için de tek örnek kilidi (door endpoint'iyle aynı yerde)
  düşünülebilir; en azından aynı issue için.
- Test: iki `Runner` aynı klasörde aynı anda çalışamaz; ikincisi anlaşılır
  hata verir.

---

## 2. P1 bulgular

### P1-1 — Edge'e dönüşen kapı hatası, hata çıktısını kaybediyor

**Kanıt:** `run.rs:942-954` önce edge'i alıyor; `take_edge`
(`run.rs:1016-1100`) yalnızca `sent-back:<to>:<trigger>` satırı yazıyor.
`register_failure`/`write_repair` çağrılmıyor; `report.result` içindeki
`reason` (kapının tail'i) atılıyor. Hedef adımın prompt'u yalnızca
`produces` artifact'i olan göndericiler için findings ekliyor
(`run.rs:2185-2206`); kapının `produces`'i yok.

**Etki:** Varsayılan `test → code` döngüsünde `code` oturumu, hangi testin
nasıl kırıldığını görmeden baştan başlıyor. Modelin düzeltme şansı düşüyor,
aynı hata tekrar ediyor, edge bütçesi boşa harcanıyor.

**Öneri:** Edge alınmadan önce kapı tail'ini `repairs/` veya yeni bir
`findings/` dosyasına yaz; `take_edge`'e `Failure` detayını taşı; hedefin
prompt'unda bu dosyayı adlandır. `review` artifact'i gibi davran.

### P1-2 — İnsan retry'ı edge bütçesini sıfırlamıyor; boş branch ve merge sonrası çökme kilitliyor

**Kanıt:**

- `run.rs:1255-1272`: `status: blocked` görülünce yalnızca `attempts`
  temizleniyor; `sent-back` satırları (edge turları) duruyor.
- `run.rs:1647-1660`: `edge_rounds` tur sayısını **tüm** satırlardan sayıyor.
- `run.rs:1016-1040`: tur > bütçe ise `Retry::Never` ile bloke.
- `run.rs:1904-1925`: `NothingToDo` (branch'ta main'e göre commit yok) →
  bloke; retry aynı branch'ı yeniden ölçer, aynı sonuç.
- `run.rs:1887-1903`: merge başarılı olduktan sonra `record_row` +
  `resolve_task`; arada çökme olursa `merged` satırı yoktur, sonraki merge
  `NothingToDo` görür.

**Etki:** "Retry" düğmesi, edge bütçesi tükenmiş bir koşuda hiçbir şey
yapmıyor; boş branch'li bir koşuda da yapamıyor. `git merge` başarılı olduğu
halde görev çözülemediyse koşu kalıcı olarak kilitli.

**Öneri:**

- Retry penceresini işaretle (`retried` satırı zaten var); `edge_rounds`
  yalnızca son retry'dan sonraki `sent-back` satırlarını saysın. Böylece
  "kişi yeni bir deneme penceresi açtı" semantiği edge'ler için de geçerli
  olur.
- `NothingToDo` durumunda `git merge-base --is-ancestor <branch> main` veya
  `git rev-list main..branch --count` ile "zaten landı mı" kontrolü yap;
  landıysa `merged` satırını yaz ve görevi çöz. Bu, merge ile kayıt arasındaki
  çökme penceresini idempotent hale getirir.
- Boş branch'te retry, `code` adımını yeniden çalıştırmıyorsa bunu UI'da
  açıkça söyle; alternatif olarak "branch boş" hatası `code`'a bir edge
  açsın (aşağıdaki P1-3 ile aynı çözüm).

### P1-3 — Rebase sonrası kapı hatasının `code`'a dönüş yolu yok

**Kanıt:** `run.rs:1982-1997`: `GateFailed` yalnızca merge attempt'ini
tekrar ediyor. `graph.rs:743-748`: merge adımının edge'i yasak. Yani
"branch kendi başına testleri geçti, main ile birleşince kırıldı" durumunda
motor yalnızca merge'i üç kez deneyip bloke ediyor; düzeltme oturumu yok.

**Etki:** Semantik çakışmalar (iki branch ayrı ayrı yeşil, birlikte kırmızı)
kalıcı blokeye dönüşüyor. Kullanıcı için tek çıkış elle worktree/main
düzeltmek.

**Öneri:** `on_conflict`'e paralel bir `on_gate_failed` (ya da
`on_merge_failed`) alanı: merge'in kapı hatasında, merge turn'ü içinde
çalışan per-task bir repair adımı. Bu adıma kapı tail'i ve rebase edilmiş
branch verilir; sonra merge yeniden denenir. `graph.rs` doğrulaması
`resolve-conflict` ile aynı şekle sahip olmalı.

### P1-4 — Merge'in sert git hataları "bloke + repair" değil, koşu hatası

**Kanıt:** `run.rs:1887`: `crate::merge::merge(...)?` — `merge()`'in
döndürdüğü `Err` doğrudan yukarı çıkıyor. `merge.rs`'te rebase'in
çakışma dışı nedenlerle başarısız olması (`worktree` kirli, yanlış branch,
git hatası) `Err` olarak dönüyor. `apply_report`'tan farklı olarak burada
repair yazılmıyor, row yazılmıyor, `run.md` durumu `running` kalırken host
`Failed` oluyor.

**Etki:** Oturum commit etmeyi unuttuysa veya worktree'de uncommitted iş
kaldıysa, koşu "anlaşılmaz bir hata" ile düşüyor; kullanıcı hangi adımın
neden durduğunu göremiyor, retry de aynı hatayı veriyor.

**Öneri:** `MergeOutcome`'a `UncleanWorktree` / `RebaseFailed` gibi
sınıflandırılmış sonuçlar ekle; `register_failure` ile repair yaz, koşuyu
`Blocked` yap. Rebase'i her durumda `--abort` ile temizle.

### P1-5 — Başarısız conflict adımı rebase'i yarım bırakıyor

**Kanıt:** `run.rs:1968-1979`: conflict adımı `Blocked` dönerse merge
`StepRun::Blocked` dönüyor; engine `git rebase --abort` çağırmıyor. Skill
(`skills/merge-conflict.md`) iyi niyetle abort talimatı veriyor, ama oturum
çökerse/başarısız olursa worktree "rebase in progress" kalıyor. Sonraki
retry'da `git rebase` "unmerged paths" ile patlayabilir (P1-4).

**Öneri:** Conflict adımından sonra, sonuç ne olursa olsun, worktree'de
`git rebase --abort` (best-effort) çalıştır; sadece başarı durumunda
rebase'in tamamlandığını doğrula.

### P1-6 — `apply_report`, `Issue::open` hatasını yutup per-task adımı grafikte "done" yapıyor

**Kanıt:** `run.rs:916-920`:

```rust
let task = report.task.and_then(|number| {
    Issue::open(&self.issue).ok().and_then(|issue| issue.task(number).cloned())
});
```

`Issue::open` bir task dosyası bozuk/eksik olduğu için hata verirse `task`
`None` olur. Sonra `record_row` satırı `-` task ile yazılır ve
`mark_done(step, None)` (`run.rs:735-746`) adımı **grafik düzeyinde** done
işaretler. Yani bir task'ın raporu, o task yokmuş gibi tüm koşuyu
etkileyebilir; join'ler erken açılır, koşu erken "done" olabilir.

**Etki:** Bozuk/geçici task dosyası (modelin atomik olmayan yazımı, dış
editör, silme) koşunun kaydını bozar. Sessiz olduğu için teşhisi zordur.

**Öneri:** `report.task` varsa `Issue::open` hatasını yutma; adımı
`Blocked` yap ve repair yaz. `mark_done(None)` yalnızca `per_task == false`
için çağrılabilsin (debug assert veya tip ayrımı).

### P1-7 — `Host::start` mevcut bir `run.md`'yi sıfırlayabilir

**Kanıt:** `src/host.rs:555-565`: yalnızca `done`/`merged` reddediliyor.
`mode == Fresh` için `prepared.resume()` kontrolü yok. `Runner::run`
(`run.rs:477-483`) taze koşuda `start()` çağırıyor; `start()` satırları,
`completed`'ı, retry'ları temizliyor (`run.rs:1125-1140`). UI'daki `plan`
bilinmeyen bir durumda (`RunState::Unknown`) `Start` dönebilir
(`src/ui/actions.rs:238-243`).

**Etki:** Var olan bir kaydın üzerine "Start" gelirse geçmiş log silinir;
eski worktree/branch'ler kalır ve yeni koşu onları yeniden kullanabilir.

**Öneri:** Fresh modda `run.md` varsa reddet (veya otomatik Resume'a çevir);
`Host::start` API'sini bu sözleşmeye göre test et.

### P1-8 — Worktree'ler kapasite kontrolünden önce ve gönderilmeyecek işler için oluşturuluyor

**Kanıt:** `run.rs:630-637`:

```rust
let workdir = ... self.step_workdir(&step, task.as_ref())?;
if busy.contains(&workdir) || jobs.len() >= self.max_parallel() {
    continue;
}
```

`step_workdir` (`run.rs:1843-1862`) gerekirse `worktree()` çağırıp
`git worktree add` yapıyor (`worktree.rs:90-135`). Yani `max_parallel = 2`
ve 8 hazır task varsa, ilk geçişte 8 worktree de **sırayla** yaratılıyor;
ikisi gönderilse bile altısı boşuna oluşturuluyor. Ayrıca worktree
oluşturma hatası, o an gönderilmeyecek bir task yüzünden tüm geçişi
düşürebiliyor.

**Etki:** İlk dispatch gecikir (git worktree add pahalıdır), disk/IO
şişer, tüm dallar aynı erken `HEAD`'ten fork eder (aşağıdaki P1-9).

**Öneri:** Önce `busy`/`max_parallel` kontrolü, sonra worktree; ya da
worktree'yi worker içinde, yalnızca gerçekten gönderilen iş için yarat
(git işlemlerini bir mutex ile serileştir). Worktree yolu `busy` için
oluşturmadan hesaplanabiliyorsa hesapla.

### P1-9 — Paralel görevler eski `main`'e göre dallanıyor; merge sırasında rebase + kapı tekrarı kaçınılmaz

**Kanıt:** `worktree.rs:90-135`: worktree, kit'in o anki `HEAD`'inden
yaratılıyor. `run.rs:1851-1862` worktree'yi ilk kullanımda yaratıp
`worktrees` map'inde tutuyor; sonraki merge'ler main'i ilerlettiğinde bu
worktree tabanı yenilenmiyor. P1-8 yüzünden tüm task worktree'leri ilk
geçişte, aynı `HEAD`'ten yaratılıyor.

**Etki:** Task 2'nin `code` oturumu, task 1 merge edildikten sonra
başlasa bile eski main üzerinde çalışır. Merge'te rebase gerekir; rebase
sonrası tüm `per_task` kapılar yeniden koşar (P1-3). Bu, "paralel işleri tek
ağaçta birleştirme" maliyetinin ana kaynağı.

**Öneri:** Worktree'yi lane gerçekten başlarken yarat (veya başlamadan önce
`main` ilerlemişse `git merge`/`rebase` ile tabanı tazele); bir task'ın
branch'ında henüz commit yoksa tazeleme güvenlidir. Alternatif: merge
kuyruğunda rebase'i ve kapıları önceden/spekülatif çalıştır (bkz. §6).

### P1-10 — UI thread'inde `prepare` + `git status`

**Kanıt:** `src/ui/run.rs` `start_run`/`resume_run`/`retry_run` →
`Host::start`/`resume` → `Host::begin` (`src/host.rs:538-583`), burada
`prepare()` (tüm task dosyalarını okur), `pin_workflow()` (yazar) ve
`require_clean()` (`git status`) **çağıran thread'de** çalışıyor.
`Host::answer` (`src/host.rs:441-477`) de yanıttan sonra `resume` çağırıp
aynı işi yapıyor.

**Etki:** Büyük repolarda `git status` ve dosya okuma UI'ı yüzlerce ms
dondurabiliyor; "cevap verince hemen devam" hissi bozuluyor.

**Öneri:** `prepare`/`pin`/`require_clean` işini worker thread'e taşı;
`begin` yalnızca hafif bir "Starting" kaydı oluşturup hatayı faza yazsın.
`answer` için de aynı.

### P1-11 — Bildirim ve "bekliyor" görünürlüğü pasif

**Kanıt:**

- Rail'de yalnızca `flow-rail-pip` sayısı var (`src/ui/chrome.rs:190-240`);
  masaüstü bildirimi, toast veya başlık çubuğunda "2 koşu seni bekliyor"
  yok. `sync_window_title` yalnızca sürülen koşu / tarama için aktivite
  yazıyor (`src/ui/window.rs:1032-1043`).
- Terminal bir koşuda açık soru kalırsa `Column::from_state` onu yine
  "Waiting on you" yapıyor (`src/store.rs:263-266`); `render_state_card`
  terminal kontrolü yapmadan soru kartını çiziyor (`src/ui/run.rs:366-390`).
  `tests/host_probe.rs:464-482` abandon'un soruyu açık bıraktığını
  doğruluyor. Yani abandon edilmiş bir koşu, cevaplanabilir soru kartıyla
  "Waiting on you"da kalıyor.
- Paralel `per_task` sorular birden fazla açık `questions/NN.md`
  üretebiliyor; store hepsini okuyor ama UI yalnızca `open_questions.first()`
  kullanıyor (`src/store.rs:947`); kartta "2 soru daha" yok.
- Soru gövdesi motor tarafından genel yazılıyor (`run.rs:2241-2252`):
  "Step approve-plan ... needs an answer". Hangi planın onaylandığı, spec
  bağlantısı veya özet yok. Soru açıkken issue kartı da çizilmiyor
  (`src/ui/run.rs:283-300`), yani spec Run sekmesinde görünmüyor.
- Bloke kartı, `run.note`'u (artifact tarama notu) stop nedeni gibi
  ekliyor (`src/ui/run.rs:644-655`); `run.note` "workflow.toml okunamadı"
  gibi bir taranmış sorundur, durma nedeni değildir.

**Etki:** Kullanıcı başka bir bölümdeyken koşunun kendisinden bir şey
istediğini fark etmeyebilir; onay kartı karar verecek bağlamı vermiyor;
birden çok soru sırayla ve sayısız görünüyor.

**Öneri:**

1. Snapshot değişiminde koşu durumlarını diff'le; `Waiting`/`Blocked`
   geçişinde (ve pencere ön planda değilse) masaüstü bildirimi
   (`notify-rust` / freedesktop) + uygulama içi toast + başlık çubuğu
   aktivitesi ("2 koşu seni bekliyor") göster. Bir ayar ve "rahatsız etme"
   desteği ekle.
2. Terminal durumlarda açık soruyu kart olarak gösterme; `Column` hesabında
   terminal durumu sorudan önce kontrol et. Abandon ederken soruyu
   `state: closed`/`cancelled` yap (motorun tek yazar kuralına uygun).
3. `open_questions` listesini kartta göster ("1/3"), her birini ayrı
   cevaplanabilir yap; hepsi cevaplanınca devam et.
4. Soru dosyasına bağlam koy: adımın `produces` artifact'inin başlığı/ilk
   paragrafı, spec yolu, task başlığı. Onay sorusunda spec'in "Change" ve
   "Acceptance criteria" bölümlerinin kısa bir özeti.
5. Bloke kartında `run.stop_reason` kullan; `run.note` yalnızca ilgili
   artifact hatası varsa ayrı bir uyarı satırı olsun.

### P1-12 — Etkin olmayan tetikler: `gate_passed` ve gate için `blocked`

**Kanıt:** `graph.rs:223-229` `GatePassed` ve `Blocked`'ı gate için geçerli
sayıyor. Ama `run.rs:942-949` gate hatasını yalnızca `GateFailed`'a
eşliyor; gate başarısında hiçbir tetik üretmiyor. `GatePassed` hiçbir yerde
`Some(...)` yapılmıyor. Yani `when = "gate_passed"` veya bir gate'te
`when = "blocked"` doğrulamadan geçer ama asla ateşlenmez.

**Etki:** Grafik yazarı sessizce ölü bir kenar tanımlar; ADR-0023'ün "kapı
başarısı da bir tetik olabilir" iddiası uygulanmamış olur.

**Öneri:** Ya bu iki tetiği uygula (`Done` → `GatePassed`; gate hatası →
hem `GateFailed` hem gerekirse `Blocked`), ya da `validate`'te
reddet. "Sessizce asla ateşlenmeyen" üçüncü bir seçenek olmamalı.

### P1-13 — Windows'ta motor kapıları hiç çalışmıyor

**Kanıt:** `gate.rs:90-116`: `#[cfg(unix)]` gerçek çalıştırma;
`#[cfg(not(unix))]` `Error::Unsupported("gates run through sh -c; the
Windows shell is not implemented yet")`. Oysa README Windows portable zip
ve installer'dan söz ediyor; `.flow/workflow.toml`'daki tüm kapılar
(`fmt`, `clippy`, `test`, `bump`) bu platformda bloke olur.

**Etki:** Windows'ta varsayılan iş akışı hiçbir koşuyu tamamlayamaz.

**Öneri:** Windows `Gate::run`'ı `run_once`'taki `cmd /C` +
`CREATE_NO_WINDOW` + `taskkill /T` yoluyla uygula; release notlarında
platform sınırını açıkça yazana kadar Windows paketini "deneysel" işaretle.

---

## 3. P2 / küçük ama gerçek bulgular

| # | Bulgu | Kanıt | Etki |
|---|---|---|---|
| 1 | Adım kimlikleri `run.md` tablosuna ve `running:` etiketine kaçışsız yazılıyor; `|`, `@`, yeni satır, virgül doğrulanmıyor | `run.rs:2518-2546` (`LogRow::parse`), `run.rs:2620` (`running_label`), `graph.rs:527-540` | `|` içeren id tabloyu, `@` içeren id `running:` eşleşmesini bozar; resume ve UI yanlış okur |
| 2 | `produces` yolunda `..`/mutlak yol doğrulanmıyor | `run.rs:2587-2612` (`artifact_name`), `graph.rs` | Issue klasörü dışına yazma/okuma; prompt da yanlış yeri gösterir |
| 3 | `RunnerConfig.factory` `None` ise `expect` panic | `run.rs:410` | Genel API'de panic; `Err` dönmeli |
| 4 | `StepOutcome::Done`'da `produces` dosyasının varlığı doğrulanmıyor (yalnızca verdict edge'lerinde okunuyor) | `run.rs:955-985`, `1525-1533` | `analyze`/`summary` hiç dosya yazmadan "done" olabilir; summary.md olmayan koşu biter |
| 5 | `max_parallel` oturum + kapı ayrımı yapmıyor | `run.rs:730-733` | 4 oturum + 4 ağır clippy aynı CPU'yu ezer; kapılar oturumları aç bırakır |
| 6 | Door zaman aşımı sabit: 600 s sessizlik, 3600 s tur | `door.rs:53-72` | Uzun bir clippy/turn 1 saatte kesilir; adım bazlı bütçe yok |
| 7 | `apply_report`'ta `Issue::open` her rapor için tüm task'ları yeniden okur | `run.rs:916-920` | Rapor sayısı × task sayısı dosya okuması; büyük koşularda gecikme |
| 8 | `write_run` her satır/durum değişiminde tüm belgeyi yeniden yazıyor | `run.rs:2336-2380` | O(R²) yazma; uzun retry geçmişinde fark edilir |
| 9 | `edge_rounds`/`has_sent_back` her çağrıda tüm satırları parse ediyor | `run.rs:1647-1678` | Edge sayısı × satır sayısı; `run.md` büyüdükçe yavaşlar |
| 10 | `run.attempts = rows.len()` | `src/store.rs:915` | "N attempts" etiketi `waiting`/`sent-back` satırlarını da sayar |
| 11 | Bloke kartındaki bütçe `DEFAULT_MAX_ATTEMPTS` sabitine gömülü | `src/ui/run.rs:659-668` | Adım bazlı bütçe eklenirse kart yalan söyler |
| 12 | `graph_done`, `ready-for-agent` olmayan bir task varsa asla tamamlanmıyor | `artifact.rs:606-627`, `run.rs:1379-1410` | `needs-info`/`ready-for-human`/`wontfix` task, summary/cleanup'ı sonsuza kadar bekletir; koşu sonda bloke olur |
| 13 | `edge_span`, hedef sender'ın atası değilse yalnızca hedefi temizliyor; hedefin zaten çalışmış torunları "done" kalıyor | `graph.rs:979-998` | "Sibling" hedefli edge'lerde iş geri dönse de aşağı akış yeniden koşmaz; doküman "dependents re-runs" diyor |
| 14 | Oturumlar issue klasörüne worker thread'inden yazıyor | `door.rs:1273-1313` | "Scheduler tek yazar" invariant'ı ihlal ediliyor; şu an dosyalar ayrık olduğu için çoğu zaman sessiz, ama kural yanlış |
| 15 | Çökme sonrası orphan oturum yönetimi yok; session id ancak oturum bitince yazılıyor | `door.rs:1273-1313` | Uygulama ölürken warp-agent'ta kalan oturum, resume'da aynı worktree'ye yazan ikinci oturumla çakışabilir |
| 16 | Uygulama ikinci örneği / CLI için tek örnek kilidi yok | `src/app.rs`, `src/host.rs` | Aynı endpoint'i iki örnek ezer; iki tarama + iki host |
| 17 | `run.md`/`retried`/`running` front matter değerlerinde virgül/`|` kaçışı yok | `run.rs:2336-2380` | Adım id'lerinde ayraç varsa alanlar bölünür (P2-1 ile aynı kök) |

---

## 4. Paralel çalışma ve "tek ağaçta birleşme" analizi

### Doğru olanlar

- **Worktree ayrımı:** `per_task` adımlar görev başına ayrı worktree'de
  çalışıyor (`worktree.rs`); `busy` kümesi aynı klasörde iki oturumu
  engelliyor (`run.rs:635`).
- **Bariyer:** Merge adımı, uçuştaki tüm işleri bekliyor ve çalışırken yeni
  iş başlatmıyor (`run.rs:589-625`). `merge()` kirli checkout'u reddettiği
  için bu doğru bir tasarım; "hızlandırmak için dirty check'i gevşetme"
  kuralı yerinde.
- **Rapor anında uygulanıyor:** Bir görev bitince `run.md` satırı ve
  `running:` hemen güncelleniyor; yavaş kardeşi beklenmiyor
  (`run.rs:773-910`). UI'da "hangi görev çalışıyor" görünür.
- **Aynı anda tek merge:** `git merge` ve rebase'ler tek scheduler
  thread'inde; `run.md` ve task durumları tek yazıcı.
- **Bağımlılık çözümü:** `Blocked by:` zinciri `ready_tasks` ile
  birleşiyor; merge bir görevi çözünce sonraki görev hazır oluyor
  (`artifact.rs:606-627`, `run.rs:1346-1410`).

### Riskler / darboğazlar

1. **Tüm worktree'ler erken yaratılıyor (P1-8)** → tüm dallar aynı `HEAD`
   üzerinde; sonraki merge'lerde rebase ve kapı tekrarı.
2. **Merge kuyruğu seri ve her görev için kapıları yeniden koşuyor**
   (`run.rs:2050-2058`, `merge.rs:168-186`). N görev = N × (fmt + clippy +
   test). En pahalı kısım bu.
3. **Rebase sonrası kapı hatası geri dönmüyor (P1-3)**; semantik çakışma
   bloke.
4. **Merge'ün temizlik kontrolü tüm ağaç (P0-2)**; motorun kendi meşru
   yazıları kuyruğu kilitliyor.
5. **`Blocked by:` olmayan ama `ready-for-agent` olmayan task'lar**
   (P2-12) koşunun kuyruğunu sonunda kilitliyor.
6. **Worktree'ler `main` ilerledikçe tazelenmiyor (P1-9)**; kod oturumu
   eski API'ye göre yazıp merge'te çakışabiliyor.
7. **Tek merge kuyruğu + `max_parallel` oturum sayısı**; merge sırasında
   hiçbir oturum çalışmıyor (bariyer). Uzun kapılarda tüm koşu duruyor.

### "Tek ağaç" için önerilen yön

Kısa vadede: `commit` protokolünü düzelt (P0-2), worktree'leri lane
başında tazele (P1-9), rebase sonrası kapı hatasını repair adımına bağla
(P1-3), `NothingToDo`'yu idempotent yap (P1-2).

Orta vadede: **kapı sonucu önbelleği** ve **toplu/spekülatif merge**:
- Her `per_task` kapı için `(gate id, ağaç hash'i, komut)` anahtarıyla
  sonuç sakla; rebase sonrası ağaç aynıysa kapıyı atla. Yalnızca **act
  etmeyen** (check) kapılar için; `bump` gibi act eden kapılar asla
  önbelleklenmemeli (ADR-0019).
- N görevi tek bir entegrasyon dalında toplayıp kapıları bir kez koş
  (GitHub merge queue / Zuul "merge group" yaklaşımı); kapı düşerse
  ikiye bölerek suçlu görevi bul. Bu, N× kapı maliyetini büyük ölçüde
  azaltır; ama görev bazlı atıf ve "her görev kendi başına yeşil" garantisi
  için ek kayıt gerekir.
- Daha az riskli ara adım: merge sırasında yalnızca **değişen** kapıları
  koş (ağaç hash'i karşılaştırması) + P1-3.

---

## 5. Retry ve hata yolları

Mevcut mekanizma:

- `max_attempts` (varsayılan 3) adım bazında; her başarısızlık
  `repairs/NN.md` yazıyor (`run.rs:1544-1595`).
- Edge'lerin ayrı tur bütçesi var (`sent-back` satırları).
- İnsan retry'ı `status: blocked` görünce attempt penceresini açıyor.
- Merge'in kendi attempt döngüsü var; conflict adımı araya giriyor.

Eksikler:

1. **Edge turu ile attempt penceresi ayrışmıyor** (P1-2). Retry yeni bir
   pencere açıyor ama edge turları eski pencereden sayılıyor.
2. **Boş branch / merge sonrası çökme** idempotent değil (P1-2).
3. **Rebase sonrası kapı hatası** düzeltme adımına bağlanmıyor (P1-3).
4. **Sert git hataları** bloke yerine koşu hatası (P1-4).
5. **Conflict adımı başarısızsa rebase temizlenmiyor** (P1-5).
6. **Geçici hatalar için backoff yok**: kapılar hemen yeniden deniyor;
   flaky testte iyi, kaynak yokluğunda değil. Deneme aralığı/jitter ve
   "retry_on" sınıflandırması (exit code / hata türü) yok.
7. **Adım bazlı bütçe yok**: `max_attempts` yalnızca `RunnerConfig`'te
   global; workflow'da `max_attempts`/`timeout` alanı yok. UI de 3'ü sabit
   varsayıyor.
8. **Orphan oturum**: çökme sonrası eski oturum iptal edilmiyor (P2-15).
9. **Retry'ın hangi adımı yeniden çalıştıracağı** UI'da net değil:
   "Retry gate" merge'teki kapıyı mı, gate adımını mı? Blocked kartı
   "fresh 3-attempt budget" diyor ama edge bütçesi ayrı.

Öneri sırası: P1-2 → P1-3 → P1-4/P1-5 → backoff + adım bazlı bütçe →
orphan oturum yönetimi.

---

## 6. Kullanıcıya bildirim / görünürlük

Bugün görünenler:

- Rail'de Runs ikonunda sayı (`need_you_count`) — Waiting + Blocked.
- Runs listesinde "Waiting on you" ve "Blocked" bantları; satırda
  "· asking you".
- Run sekmesinde soru kartı, bloke kartı, deneme geçmişi, onarım özeti,
  oturumlar, motor log'u.
- Başlık çubuğunda yalnızca sürülen koşu/tarama aktivitesi.

Eksikler ve öneriler:

| Konu | Bugün | Öneri |
|---|---|---|
| Koşu seni bekliyor | Yalnızca rail pip + liste | Snapshot diff'inde `Waiting/Blocked` geçişi yakala; toast + masaüstü bildirimi + başlık "2 koşu seni bekliyor" |
| Soru bağlamı | Genel "needs an answer" gövdesi | Soru dosyasına spec/task bağlamı ve artifact özeti; kartta "Spec'i aç" |
| Çoklu soru | Yalnızca ilk açık soru | "1/3" ve sırayla cevaplama; hepsi kapanınca devam |
| Terminal + açık soru | Abandon edilen koşu "Waiting on you"da kalır | Terminal durumda soru kartını gizle; abandon'da soruyu kapat |
| Bloke nedeni | `run.note` (tarama notu) karışabiliyor | `stop_reason` + son repair; `note` ayrı uyarı |
| Deneme sayısı | `rows.len()` | Gerçek attempt sayısı; sent-back/waiting hariç |
| Canlı oturum | Yalnızca bitince `sessions/` | İstersen canlı transcript/event akışı (opsiyonel) |
| OS bildirimi | Yok | `notify-rust` / freedesktop, DND ve ayar desteğiyle |

Masaüstü bildirimi, uygulama ön planda değilken özellikle önemli; mevcut
rail pip'i kullanıcı Runs dışındaysa fark edilmeyebilir.

---

## 7. "Yapay zekâ kendisi karar versin" — bugünkü durum ve tasarım önerisi

### Bugün modelin karar verebildiği yerler

1. **`verdict`**: `review` artifact'inde `ready`/`changes`; `changes` bir
   edge'i tetikler (`run.rs:1597-1645`, ADR-0016/0023).
2. **Görev dosyaları**: `analyze` task'ları, `Blocked by:` zincirini ve
   `Status`'ü yazar; bu, paralelliği ve sırayı dolaylı olarak belirler
   (`artifact.rs`, `skills/analyze.md`).
3. **Soru cevabı**: insan cevabı `answer_no` edge'ini tetikleyebilir
   (ancak P0-1).

### Modelin karar veremediği / akışı şekillendiremediği yerler

- **Dinamik adım ekleme**: Plan dokümanı `research` ve `repair`
  düğümlerinin "motor tarafından eklenebileceğini" söylüyor
  (`docs/plans/warp-flow.md:230`); kodda böyle bir step kind veya ekleme
  yolu yok. Grafik başlangıçta pinleniyor ve çalışma boyunca sabit
  (`prepare::pin_workflow`).
- **Oturum içi soru**: Plan dokümanı "bir adım veya harness agent'ı
  tarafından sorulan" sorulardan söz ediyor (`docs/plans/warp-flow.md:71`);
  motor yalnızca kendi yazdığı `approve` sorusunu üretiyor
  (`run.rs:51-65`, `2227-2256`). UI'da `select`/`multi`/`text` dalları var
  (`src/ui/run.rs:498-560`) ama motor bu türleri hiç yazmıyor.
- **Retry politikası**: Model "bu adımı şu nedenle tekrar dene" diyemez;
  edge'ler ve bütçeler grafikte sabit.
- **Paralellik**: Model yalnızca task `Blocked by:` ile dolaylı etki
  edebiliyor; adım/oturum bazlı eşzamanlılık seçemiyor.
- **İzin politikası**: Plan decision 12 (worktree dışına yazma/network/
  push → soru) uygulanmamış; motor harness'a sandbox/policy geçmiyor.

### Önerilen üç seam (motorun güvenlik modelini bozmadan)

1. **Plan artifact'i / dinamik grafik**: `analyze` (veya yeni bir `plan`
   adımı) `plan.toml` yazsın: adım listesi, bağımlılıklar, `per_task`,
   `max_parallel`, bütçeler. Motor bunu şema + döngüsellik + skill
   doğrulamasından geçirip çalışma grafiğine eklesin; şablonun sabit
   kapıları/merge'i korunur. Model böylece "şu iki iş paralel, şu iş
   blocker" kararını açıkça verir; motor yine tek yürütücü olur.
   Sınırlar: maksimum adım/task sayısı, toplam bütçe, yalnızca bilinen
   skill'ler, `depends_on` döngüsü yok.
2. **Sorular için dosya protokolü**: Harness adımı
   `questions/request-NN.md` yazsın: `kind`, `options`, `title`, `body`,
   `asked_by`, `state: requested`. Motor adım sınırında bunu görüp
   `state: open` yapsın ve koşuyu `waiting`'e alsın. UI zaten
   select/multi/text çizebiliyor; bu, en küçük ve en yüksek değerli
   değişiklik. Model ne zaman insana soracağını kendisi belirler; motor
   yalnızca sınırda okur (ADR-0008'in ruhuna uygun).
3. **Politika alanları**: `Step`'e `max_attempts`, `timeout_secs`,
   `retry_on`, `on_blocked`/`on_gate_failed` ekle; model bunları plan
   artifact'inde önerebilir, motor sınırlar içinde uygular. Modelin
   kendi kendine "başarılıyım" demesi yerine yine dosya/exit code
   tetikleri kullanılmalı (ADR-0008).

Bu üçü, "AI karar verir, motor uygular" ayrımını korur: karar dosyada
görünür ve doğrulanabilir olur; motorun retry/merge/bütçe otoritesi
sarsılmaz.

---

## 8. Optimizasyon önerileri (öncelik ve beklenen etki)

| # | Öneri | Nereye | Beklenen etki | Risk |
|---|---|---|---|---|
| 1 | Commit protokolünü tamamla (start + final + `.scratch` kapsamı) | `run.rs`, `merge.rs` | P0-2'yi çözer; sonraki koşuların merge'ü açılır | Düşük |
| 2 | Süreçler arası `flock` kilidi | `run.rs`, `host.rs` | P0-3; eşzamanlı motor yarışı biter | Düşük |
| 3 | Edge bütçesini retry penceresine bağla; `NothingToDo`'yu uzlaştır | `run.rs` | Retry'ı işlevsel yapar | Düşük |
| 4 | Kapı tail'ini findings/repair dosyasına yaz | `run.rs`, prompt | `test→code` döngüsü kör kalmaz | Düşük |
| 5 | Worktree'yi dispatch anında yarat/tazele | `run.rs`, `worktree.rs` | İlk dispatch hızlanır; rebase azalır | Orta |
| 6 | Ağaç hash'li kapı önbelleği (yalnız check kapıları) | `merge.rs`, `gate.rs` | Merge başına kapı maliyeti düşer | Orta (act kapıları hariç) |
| 7 | Toplu/spekülatif merge (merge group) | `merge.rs`, `run.rs` | N× kapı → ~1×; büyük hız | Yüksek (atıf/rollback) |
| 8 | Taramayı artımlı yap: `notify` + mtime cache; 2 sn'lik `git status`'u kaldır | `store.rs` | CPU/IO ve UI gecikmesi düşer | Orta |
| 9 | `run.md` için append-only log / JSONL yan dosya | `run.rs` | O(R²) yazma ve tam parse azalır | Orta (protokol) |
| 10 | Edge tur sayaçlarını bellekte tut, satırları tekrar parse etme | `run.rs` | Uzun koşularda scheduler hızlanır | Düşük |
| 11 | Kritik yol öncelikli zamanlama + ayrı oturum/kapı bütçeleri | `run.rs` | Duvarsaati süresi düşer; CPU taşması azalır | Orta |
| 12 | Door katalog cevabını koşu başına cache'le | `door.rs` | Her adımda 2 istek azalır | Düşük |
| 13 | Adım bazlı timeout/attempt; door reconnect | `graph.rs`, `door.rs` | Uzun turn'ler kesilmez; geçici kopmalar toparlanır | Orta |
| 14 | Bildirim servisi + terminal soru temizliği | `store.rs`, `ui/*` | Kullanıcı kaçırmaz; yanlış "waiting" biter | Düşük |
| 15 | Gözlemlenebilirlik: adım süresi, kuyruk beklemesi, kapı süresi, oturum token'ı | `run.rs`, `store.rs` | Optimizasyonlar ölçülebilir olur | Düşük |

### Endüstri pratiği notları

- **Merge queue / merge group**: GitHub, merge queue'da gerekli kontrolleri
  geçici bir merge grubu üzerinde çalıştırır; Zuul ise değişiklikleri
  "speculative execution" ile, öndeki değişiklikler merge olacak varsayımıyla
  paralel test eder ve biri düşerse etkilenenleri yeniden koşar. warp-flow'un
  bariyeri doğru ama tek tek merge ettiği için bu paralellikten yararlanmıyor.
  Kaynak: [GitHub merge queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue),
  [Zuul project gating](https://zuul-ci.org/docs/zuul/latest/gating.html).
- **İçerik hash'li cache**: Turborepo/Bazel, görev çıktılarını girdilerin
  hash'iyle anahtarlar; aynı girdi → aynı sonuç. Kapılar için "ağaç hash'i"
  aynı fikrin doğal uygulamasıdır; ancak act eden kapılar (bump) bu kuralın
  dışında tutulmalı. Kaynak:
  [Turborepo caching](https://turborepo.dev/docs/crafting-your-repository/caching),
  [Bazel remote caching](https://bazel.build/remote/caching).
- **Retry + idempotency**: AWS'nin retry/backoff rehberi, tekrar denenen
  işlemin idempotent olmasını şart koşar; aksi halde kısmi güncellemeler
  durumu bozar. warp-flow'ta merge/resolve adımı tam da bu yüzden idempotent
  olmalı. Kaynak:
  [AWS retry with backoff](https://docs.aws.amazon.com/prescriptive-guidance/latest/cloud-design-patterns/retry-backoff.html).
- **Bildirim**: Linux'ta freedesktop `org.freedesktop.Notifications`;
  Rust'ta `notify-rust`. Kaynak:
  [freedesktop notification spec](https://specifications.freedesktop.org/notification/latest/),
  [notify-rust](https://docs.rs/notify-rust/latest/notify_rust/).
- **Dosya izleme**: `notify` crate'i inotify/poll watcher sunar; 2 sn'lik
  tam tarama yerine olay + debounce. Kaynak:
  [notify](https://docs.rs/notify/latest/notify/).

---

## 9. Önerilen yol haritası

**Sprint 1 — doğruluk (P0):**

1. P0-1: "no" + edge yok → bloke/repair; `validate` ile zorunlu kıl.
2. P0-2: `flow: issue` + kapanış commit'i; merge temizlik kapsamı;
   `.scratch`/`.flow` politikası.
3. P0-3: `flock` tabanlı koşu kilidi; UI'da "başka süreç sürüyor".

**Sprint 2 — retry ve merge dayanıklılığı:**

4. P1-2 (edge penceresi + `NothingToDo` uzlaştırma).
5. P1-3/P1-4/P1-5 (merge hata sınıfları, `on_gate_failed`, abort).
6. P1-1 (kapı findings dosyası).
7. P1-6 (`Issue::open` hatası), P1-7 (Fresh koruması).

**Sprint 3 — görünürlük:**

8. P1-11 (bildirim, çoklu soru, terminal soru, bağlam).
9. P1-12 (ölü tetikler), P2-10/11 (UI sayıları).

**Sprint 4 — performans:**

10. P1-8/P1-9 (worktree), P0-2 sonrası kapı cache, tarama artımlılığı.
11. Toplu/spekülatif merge (opsiyonel, feature flag).

**Sprint 5 — AI'nın akışı şekillendirmesi:**

12. Plan artifact'i, ad-hoc soru protokolü, adım politikaları.

---

## 10. Test boşlukları

Mevcut 200 motor testi ve app probe'ları iyi; ancak aşağıdaki yollar
test edilmiyor:

- `answer_no` edge'i olmayan soruya "no" → **done olmamalı**.
- Koşu bittikten sonra `git status` temiz; ikinci koşu merge'e ulaşır.
- Koşu sırasında `.flow/workflow.toml` kaydı merge'ü bloke etmez.
- Gate edge'i alındığında findings dosyası yazılır ve hedef prompt'ta geçer.
- İnsan retry'ı edge turlarını sıfırlar.
- `NothingToDo` + branch zaten merge edilmiş → görev çözülür.
- Merge sırasında rebase hatası → bloke + repair (koşu hatası değil).
- Conflict adımı başarısız → worktree'de rebase kalmaz.
- `Issue::open` bozuk task → per-task adım grafikte done olmaz.
- `Host::start` mevcut `run.md` üzerinde reddeder.
- `gate_passed` ve gate `blocked` tetikleri ya çalışır ya doğrulamada
  reddedilir.
- Terminal koşu + açık soru → Waiting on you olmaz.
- Aynı issue'da iki `Runner` → ikincisi kilit hatası verir.
- Windows gate yolu (platform CI'da).

Bu testler, mevcut davranışın çoğunu bozmadan eklenebilir; çoğu fake
harness + temp repo ile saniyeler içinde koşar.

---

## 11. Sonuç

Motorun omurgası (worktree ayrımı, bariyer, artifact tabanlı kararlar,
tek yazıcı scheduler, kapsamlı repair kaydı) doğru. Sorunlar daha çok
**protokolün kapanışında** (commit, kilit, retry penceresi), **hata
bilgisinin kaybolmasında** (gate edge, merge hataları) ve **görünürlükte**
(toplu bildirim, soru bağlamı) yoğunlaşıyor.

P0'lar düzeltilmeden "paralel işler güvenle tek ağaçta birleşiyor"
denemez: ilk koşudan sonra merge kuyruğu kirlilik yüzünden kilitlenebilir,
"Hayır" onay sayılabilir ve iki motor aynı klasöre yazabilir. Bu üçü
çözüldükten sonra kapı cache'i ve toplu merge, en yüksek getirili
optimizasyonlardır.
