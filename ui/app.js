'use strict';
const $ = id => document.getElementById(id);
const fragment = location.hash.slice(1);
if (/^[a-f0-9]{64}$/.test(fragment)) {
  sessionStorage.setItem('mpd-bot-token', fragment);
  history.replaceState(null, '', location.pathname);
}
const token = sessionStorage.getItem('mpd-bot-token') || '';
let keys = {}, dirty = false, pending = false;
const fields = ['enabled','twitch_enabled','twitch_username','twitch_channel','model','endpoint','bot_name','prompt','command','cooldown_seconds','memory_turns','max_conversations','max_output_tokens','max_reply_chars'];
function notice(message, error=false) { $('notice').textContent=message; $('notice').classList.toggle('error',error); $('notice').hidden=false; }
async function api(path, method='GET', body) {
  const response = await fetch(path,{ method, headers:{Authorization:`Bearer ${token}`, ...(body === undefined ? {} : {'Content-Type':'application/json'})}, ...(body === undefined ? {} : {body:JSON.stringify(body)}) });
  const data = await response.json().catch(()=>({error:`Request failed (${response.status})`}));
  if (response.status===401) { $('auth-panel').hidden=false; $('workspace').hidden=true; }
  if (!response.ok) throw new Error(data.error || `Request failed (${response.status})`);
  return data;
}
function keyState() {
  const provider = $('provider_select').value;
  for (const [id,node] of [[provider,'api-key-state'],['twitch','twitch-key-state']]) {
    const key=keys[id]; $(node).textContent=key?.configured ? `Configured · ${key.source}` : 'Not configured';
  }
  $('endpoint-field').hidden=provider!=='compatible';
}
function readConfig() {
  const config={};
  for (const name of fields) { const el=$(name); config[name]=el.type==='checkbox' ? el.checked : el.type==='number' ? Number(el.value) : el.value.trim(); }
  config.personality=$('personality_text').value;
  config.provider=$('provider_select').value;
  config.excluded_users=$('excluded_users').value.split(',').map(s=>s.trim().toLowerCase()).filter(Boolean);
  return config;
}
async function load() {
  if (!token) { $('auth-panel').hidden=false; $('workspace').hidden=true; return; }
  try {
    const data=await api('/api/config');
    for (const name of fields) { const el=$(name); if(el.type==='checkbox') el.checked=data.config[name]; else el.value=data.config[name]; }
    $('personality_text').value=data.config.personality;
    $('provider_select').value=data.config.provider;
    $('excluded_users').value=data.config.excluded_users.join(', ');
    $('preview-bot').textContent=data.config.bot_name;
    keys=data.keys; keyState();
    if(data.warning) notice(data.warning);
    await refreshStatus();
  } catch(error) { notice(error.message,true); }
}
async function refreshStatus() {
  if(document.hidden || !token || $('workspace').hidden) return;
  try {
    const status=await api('/api/status');
    $('twitch-status').textContent=status.twitch;
    $('engine-status').textContent=status.busy ? 'Working…' : 'Ready';
    if(status.replies!==undefined) $('reply-count').textContent=status.replies;
    if(status.conversations!==undefined) $('memory-count').textContent=status.conversations;
    if(status.last_error!==undefined) { $('last-error').hidden=!status.last_error; $('last-error').textContent=status.last_error||''; }
  } catch(error) { $('engine-status').textContent='Unavailable'; notice(error.message,true); }
}
async function action(button, work) {
  if(pending) return;
  pending=true; button.disabled=true;
  try { await work(); } catch(error) { notice(error.message,true); } finally { pending=false; button.disabled=false; }
}
$('settings').addEventListener('input',()=>{ dirty=true; $('save-state').textContent='You have unsaved changes.'; });
$('provider_select').addEventListener('change',()=>{ $('api-key').value=''; keyState(); });
$('settings').addEventListener('submit',event=>{
  event.preventDefault();
  action($('save-settings'),async()=>{ await api('/api/config','PUT',readConfig()); dirty=false; $('save-state').textContent='All changes saved.'; $('preview-bot').textContent=$('bot_name').value; notice('Settings saved. Conversation memory cleared; Twitch will apply the new settings.'); await refreshStatus(); });
});
async function keyAction(twitch, remove=false) {
  const id=twitch?'twitch':$('provider_select').value;
  const input=$(twitch?'twitch-key':'api-key');
  const remember=$(twitch?'remember-twitch':'remember-api').checked;
  const button=$(remove?(twitch?'remove-twitch-key':'remove-api-key'):(twitch?'save-twitch-key':'save-api-key'));
  await action(button,async()=>{
    const data=await api('/api/keys','POST',{id,key:input.value,remember,remove});
    input.value=''; keys=data.keys; keyState();
    notice(remove?'Credential removed from this session and OS storage. Environment variables, if set, take effect again at restart.':remember?'Credential saved in the OS credential store.':'Credential saved for this session. A previously saved key, if any, remains in OS storage.');
  });
}
$('save-api-key').onclick=()=>keyAction(false);
$('save-twitch-key').onclick=()=>keyAction(true);
$('remove-api-key').onclick=()=>keyAction(false,true);
$('remove-twitch-key').onclick=()=>keyAction(true,true);
$('test-reply').onclick=()=>action($('test-reply'),async()=>{
  if(dirty) throw new Error('Save your settings before testing the personality.');
  const message=$('preview-input').value.trim(); if(!message) throw new Error('Enter a test message.');
  $('preview-user').textContent=message; $('preview-reply').textContent='Thinking…';
  try { const result=await api('/api/preview','POST',{platform:'preview',channel:'preview',user:'you',message}); $('preview-reply').textContent=result.reply || `Skipped: ${result.skipped}. Try again shortly.`; }
  catch(error) { $('preview-reply').textContent='No reply generated.'; throw error; }
});
$('clear-memory').onclick=()=>action($('clear-memory'),async()=>{ await api('/api/memory/clear','POST',{}); notice('Conversation memory cleared.'); await refreshStatus(); });
window.addEventListener('beforeunload',event=>{ if(dirty){ event.preventDefault(); event.returnValue=''; } });
document.addEventListener('visibilitychange',()=>{ if(!document.hidden) refreshStatus(); });
async function poll() { await refreshStatus(); setTimeout(poll,5000); }
load().then(()=>setTimeout(poll,5000));
