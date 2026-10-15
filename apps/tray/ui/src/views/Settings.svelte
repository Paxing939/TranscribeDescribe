<script lang="ts">
  // F-102: settings window; on first run it is the wizard (F-103).
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { inTauri } from '../lib/api';

  type Form = Record<string, any>;
  type Check = { name: string; level: 'ok' | 'warn' | 'error'; detail: string };

  let f = $state<Form | null>(null);
  let saving = $state(false);
  let msg = $state('');
  let checks = $state<Check[] | null>(null);
  let checking = $state(false);

  const mock: Form = {
    callsDir: '~/Videos/Calls', userName: 'Илья', autostart: true, detectionEnabled: true, sourceWindow: true,
    sourceMic: true, defaultMode: 'ask', obsPasswordMode: 'auto', obsExe: 'C:/Program Files/obs-studio/bin/64bit/obs64.exe',
    defaultRecordMode: 'audio', modelGpu: 'large-v3-turbo', modelCpu: 'medium', device: 'auto', language: 'ru',
    glossary: 'CI/CD, пайплайн', llmPreset: 'deepseek', llmBaseUrl: 'https://api.deepseek.com', llmModel: 'deepseek-flash',
    allowCloud: true, hasLlmKey: false, useLocalFallback: false, autoApplyMin: 10, transliterate: false,
    overlayPosition: 'bottom-right', recordingPill: 'full', excludeFromCapture: true, remindConsent: false,
    minFreeGb: 5, deleteVideoAfterDays: 0, pipelineMode: 'local', firstRun: true,
  };

  onMount(async () => {
    try { f = inTauri ? await invoke<Form>('settings_get') : mock; }
    catch (e) { msg = String(e); }
  });

  async function save() {
    if (!f) return;
    saving = true; msg = '';
    try {
      const form = { ...f, llmApiKey: f.llmApiKey || null, obsPassword: f.obsPassword || null };
      if (inTauri) await invoke('settings_save', { form });
      msg = 'Сохранено';
      if (f.llmApiKey) { f.hasLlmKey = true; f.llmApiKey = ''; }
      f.obsPassword = '';
    } catch (e) { msg = String(e); }
    finally { saving = false; }
  }

  async function doctor() {
    checking = true; checks = null;
    try { checks = inTauri ? await invoke<Check[]>('settings_doctor') : [{ name: 'OBS', level: 'warn', detail: 'mock' }]; }
    catch (e) { msg = String(e); }
    finally { checking = false; }
  }

  function onPreset() {
    if (!f) return;
    if (f.llmPreset === 'deepseek') { f.llmBaseUrl = 'https://api.deepseek.com'; f.llmModel ||= 'deepseek-flash'; f.allowCloud = true; }
    if (f.llmPreset === 'ollama') { f.llmBaseUrl = 'http://127.0.0.1:11434'; f.llmModel = f.llmModel?.includes('deepseek') ? 'qwen2.5:7b' : (f.llmModel || 'qwen2.5:7b'); }
  }
  const cloudPreset = $derived(f && ['deepseek', 'anthropic'].includes(f.llmPreset));
</script>

<div class="page">
  {#if !f}
    <p class="muted">{msg || 'Загрузка…'}</p>
  {:else}
    {#if f.firstRun}
      <div class="welcome">
        <h1>Добро пожаловать в CallScribe</h1>
        <p>Проверьте основные настройки и нажмите «Сохранить». Перед первой записью настройте OBS
          (ТЗ §14): включите сервер WebSocket в <i>Инструменты → Настройки сервера WebSocket</i>, затем выполните
          <code>callscribe obs-setup</code> — он создаст сцены и профиль «только звук».</p>
      </div>
    {/if}

    <section>
      <h2>Общие</h2>
      <label>Папка звонков <input bind:value={f.callsDir} /></label>
      <label>Ваше имя (для «Я» в саммари) <input bind:value={f.userName} /></label>
      <label class="check"><input type="checkbox" bind:checked={f.autostart} /> Запускать при входе в Windows</label>
    </section>

    <section>
      <h2>Детект звонков</h2>
      <label class="check"><input type="checkbox" bind:checked={f.detectionEnabled} /> Отслеживать звонки</label>
      <label class="check"><input type="checkbox" bind:checked={f.sourceWindow} /> По окнам встреч (Zoom, Teams, Webex, Meet)</label>
      <label class="check"><input type="checkbox" bind:checked={f.sourceMic} /> По занятому микрофону (Telegram, Discord, браузер…)</label>
      <label>Для неизвестных приложений
        <select bind:value={f.defaultMode}>
          <option value="ask">спрашивать</option><option value="auto">записывать сразу</option>
          <option value="record_then_ask">записывать и спрашивать</option><option value="ignore">не реагировать</option>
        </select>
      </label>
    </section>

    <section>
      <h2>OBS</h2>
      <label>obs64.exe <input bind:value={f.obsExe} /></label>
      <label>Пароль websocket
        <select bind:value={f.obsPasswordMode}>
          <option value="auto">из настроек OBS (auto)</option><option value="keyring">свой, в хранилище Windows</option>
        </select>
      </label>
      {#if f.obsPasswordMode === 'keyring'}<label>Пароль <input type="password" bind:value={f.obsPassword} placeholder="оставьте пустым, чтобы не менять" /></label>{/if}
      <label>Режим записи по умолчанию
        <select bind:value={f.defaultRecordMode}><option value="audio">только звук</option><option value="screen">звук + экран</option></select>
      </label>
    </section>

    <section>
      <h2>Распознавание</h2>
      <div class="grid">
        <label>Модель на GPU <input bind:value={f.modelGpu} /></label>
        <label>Модель на CPU <input bind:value={f.modelCpu} /></label>
        <label>Устройство <select bind:value={f.device}><option value="auto">авто</option><option value="gpu">GPU</option><option value="cpu">CPU</option></select></label>
        <label>Язык <select bind:value={f.language}><option value="ru">русский</option><option value="en">английский</option><option value="auto">авто</option></select></label>
      </div>
      <label>Глоссарий (через запятую) <input bind:value={f.glossary} /></label>
    </section>

    <section>
      <h2>Саммари (LLM)</h2>
      <label>Провайдер
        <select bind:value={f.llmPreset} onchange={onPreset}>
          <option value="deepseek">DeepSeek (облако)</option><option value="ollama">Ollama (локально)</option>
          <option value="openai_compatible">OpenAI-совместимый сервер</option><option value="anthropic">Anthropic (облако)</option>
          <option value="none">без саммари</option>
        </select>
      </label>
      {#if f.llmPreset !== 'none'}
        {#if f.llmPreset !== 'deepseek'}<label>Адрес <input bind:value={f.llmBaseUrl} /></label>{/if}
        <label>Модель <input bind:value={f.llmModel} /></label>
        {#if f.llmPreset !== 'ollama'}
          <label>Ключ API <input type="password" bind:value={f.llmApiKey} placeholder={f.hasLlmKey ? 'сохранён — оставьте пустым, чтобы не менять' : 'не задан'} /></label>
        {/if}
        <label class="check"><input type="checkbox" bind:checked={f.allowCloud} /> Разрешить облачную LLM (транскрипт уходит на сервер провайдера)</label>
        {#if cloudPreset}<p class="warn">Это облако: текст транскрипта отправляется на серверы провайдера.</p>{/if}
        <label class="check"><input type="checkbox" bind:checked={f.useLocalFallback} /> Для звонков «не отправлять в облако» — локальная Ollama</label>
      {/if}
    </section>

    <section>
      <h2>Названия и хранение</h2>
      <div class="grid">
        <label>Применять название через, мин <input type="number" min="0" bind:value={f.autoApplyMin} /></label>
        <label>Мин. свободно на диске, ГБ <input type="number" min="0" bind:value={f.minFreeGb} /></label>
        <label>Удалять записи старше, дней (0 — никогда) <input type="number" min="0" bind:value={f.deleteVideoAfterDays} /></label>
        <label>Обработка
          <select bind:value={f.pipelineMode}><option value="local">на этой машине</option><option value="record_only">только запись</option><option value="remote">на другой машине</option></select>
        </label>
      </div>
      <label class="check"><input type="checkbox" bind:checked={f.transliterate} /> Транслитерировать названия папок</label>
    </section>

    <section>
      <h2>Оверлеи</h2>
      <div class="grid">
        <label>Угол экрана
          <select bind:value={f.overlayPosition}>
            <option value="bottom-right">снизу справа</option><option value="bottom-left">снизу слева</option>
            <option value="top-right">сверху справа</option><option value="top-left">сверху слева</option>
          </select>
        </label>
        <label>Плашка записи <select bind:value={f.recordingPill}><option value="full">полная</option><option value="dot">точка</option><option value="hidden">скрыта</option></select></label>
      </div>
      <label class="check"><input type="checkbox" bind:checked={f.excludeFromCapture} /> Скрывать оверлеи из записи и демонстрации экрана</label>
      <label class="check"><input type="checkbox" bind:checked={f.remindConsent} /> Напоминать предупредить собеседников о записи</label>
    </section>

    {#if checks}
      <section>
        <h2>Проверка системы</h2>
        {#each checks as c}<div class="chk {c.level}"><b>{c.name}:</b> {c.detail}</div>{/each}
      </section>
    {/if}
  {/if}
</div>
{#if f}
  <div class="bar">
    <span class="muted">{msg}</span>
    <span class="spacer"></span>
    <button onclick={doctor} disabled={checking}>{checking ? 'Проверяю…' : 'Проверить систему'}</button>
    <button class="primary" onclick={save} disabled={saving}>Сохранить</button>
  </div>
{/if}

<style>
  .page { height: calc(100% - 52px); overflow: auto; padding: 12px 16px 24px; user-select: text; -webkit-user-select: text; }
  .welcome { border: 1px solid var(--accent); border-radius: 10px; padding: 10px 14px; margin-bottom: 8px; }
  h1 { font-size: 16px; margin: 0 0 6px; }
  h2 { font-size: 13px; margin: 16px 0 6px; color: var(--muted); text-transform: uppercase; letter-spacing: .04em; }
  section { display: flex; flex-direction: column; gap: 6px; }
  label { display: flex; flex-direction: column; gap: 3px; font-size: 12px; color: var(--muted); }
  label.check { flex-direction: row; align-items: center; gap: 6px; color: var(--fg); font-size: 13px; }
  input:not([type=checkbox]), select { font: inherit; font-size: 13px; color: var(--fg); background: var(--input-bg); border: 1px solid var(--border); border-radius: 6px; padding: 5px 8px; }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 6px 12px; }
  .warn { color: var(--danger); font-size: 12px; margin: 0; }
  .muted { color: var(--muted); font-size: 12px; }
  .chk { font-size: 12px; padding: 2px 0; }
  .chk.warn { color: #d97706; } .chk.error { color: var(--danger); }
  .bar { position: fixed; left: 0; right: 0; bottom: 0; height: 52px; display: flex; gap: 8px; align-items: center; padding: 0 16px; border-top: 1px solid var(--border); background: var(--bg); }
  .spacer { flex: 1; }
  code { font-size: 12px; }
</style>
