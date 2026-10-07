# haste — план и структура

Лёгкий нативный аудиоплеер для Debian 13 (Trixie) на Rust + GTK4.
Компоновка «панель плеера сверху + плейлист» вдохновлена классическими
плеерами, интерфейс и ресурсы — оригинальные.

## Стек и обоснование зависимостей

| Крейт | Зачем | Фичи |
|---|---|---|
| `gtk4` 0.11 | GUI, динамически линкуется с системным libgtk-4 | `default-features = false`, `v4_12` (FileDialog, ColumnView::scroll_to) |
| `symphonia` 0.6 | декодирование + теги + обложки | `mp3 flac ogg vorbis wav pcm aac isomp4 id3v1 id3v2` |
| `cpal` 0.18 | вывод звука через ALSA (на Trixie → PipeWire/Pulse через alsa-plugins) | `default-features = false` |

Всё остальное — своё: SPSC-кольцевой буфер, линейный ресемплер (фолбэк),
ГПСЧ xorshift для shuffle, парсер/писатель M3U, формат сессии
(key=value + TSV-кэш плейлиста), percent-decoding `file://` URI,
biquad-эквалайзер (этап 2). Никаких tokio/serde/rand/ringbuf/lofty.

Связь фоновых потоков с UI: `std::sync::mpsc` + `glib::MainContext::invoke`
(будит главный цикл и вычитывает канал), без `async-channel`.

## Структура

```
Cargo.toml                 профиль release под размер
.cargo/config.toml         -C target-cpu=x86-64, --gc-sections, --as-needed
src/
  main.rs                  точка входа
  util.rs                  формат времени, xorshift, percent-decode, пути
  audio/
    mod.rs                 Player (команды), Event, общее состояние
    ring.rs                lock-free SPSC буфер f32 с «flush» без блокировок
    engine.rs              поток декодирования: команды, seek, конец трека
    decoder.rs             symphonia → f32 interleaved stereo
    output.rs              cpal-поток; колбэк без аллокаций и блокировок
    resample.rs            линейный ресемплер (если устройство не умеет rate)
    eq.rs                  (этап 2) 10-полосный biquad-эквалайзер
  playlist/
    mod.rs                 Track, сортировка, фильтр, порядок (shuffle/repeat)
    m3u.rs                 импорт/экспорт M3U/M3U8
  library.rs               чтение тегов, рекурсивный обход, пул сканеров
  config.rs                ~/.config/haste/{session.conf,playlist.tsv}
  ui/
    mod.rs                 Application, окно, горячие клавиши, DnD
    panel.rs               панель «Сейчас играет»
    list.rs                gio::ListStore → SortListModel → FilterListModel → ColumnView
data/
  io.github.markzorg.Haste.desktop
  io.github.markzorg.Haste.svg
packaging/build-deb.sh     сборка .deb через dpkg-deb
scripts/size-report.sh     release + cargo bloat + размер
```

## Потоки

```
 UI (GTK main loop) ──Command──▶ engine thread ──samples──▶ SPSC ring ──▶ cpal callback
        ▲                            │                                     (без аллокаций,
        └──────── Event (mpsc + MainContext::invoke) ◀─────┘                без локов)
 scanner threads (N≤4) ──batches Track──▶ UI
```

* Колбэк: читает кольцо, применяет громкость с плавным изменением (без щелчков
  при паузе), считает прочитанные сэмплы (позиция). Seek = атомарный маркер
  `flush_to` — колбэк сам перепрыгивает старые данные, без блокировок.
* Конец трека: движок отмечает индекс конца, ждёт, пока колбэк его проиграет,
  и только тогда шлёт `Event::Finished` — хвост трека не обрезается.
* Частота: сначала пытаемся открыть устройство на частоте файла (PipeWire
  ресемплирует качественно), иначе — дефолтная конфигурация + свой ресемплер.

## Плейлист на 50k+

`gio::ListStore<BoxedAnyObject(Rc<Track>)>` → `SortListModel` (сортер
ColumnView) → `FilterListModel` (incremental, `CustomFilter` по
предвычисленному lowercase-ключу) → `MultiSelection` → `ColumnView`.
Теги сканируются до вставки (пачками), поэтому строки не обновляются
поштучно. Сессия хранит теги в TSV-кэше — повторного сканирования при
запуске нет.

## Этапы

1. Скелет: окно + воспроизведение одного файла, замер размера.
2. Плейлист: модель, колонки, сортировка, фильтр, DnD, папки, удаление.
3. Транспорт: next/prev, shuffle, repeat, seek-слайдер, громкость.
4. Теги + обложка в панели.
5. M3U/M3U8 импорт/экспорт (+ тесты).
6. Горячие клавиши, медиаклавиши.
7. Сессия.
8. .desktop, иконка, MIME, .deb.
9. Этап 2 (отдельные коммиты): EQ; `mpris` (feature, zbus); `tray` (feature, SNI).

После каждого пункта: `cargo clippy`, `cargo build --release`, замер размера.
