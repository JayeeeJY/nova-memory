'use strict';
let nativeView='space',nativeActive=false,nativeBusy=false,nativeSignature='',connectionIssueId=crypto.randomUUID(),issuedConfiguration=null,issuedHost=null,issuedConnectionId=null,issuedConnectionMeta=null,gatewayViewState=null,usageBefore=null,searchTimer,setupEpoch=0,connectionInventory=null;
const presetLabels={read_only:'只读参考',topic:'限定主题',capture_read:'采集与读取'};
const connectionHostLabels={chatgpt:'ChatGPT',codex:'Codex',claude:'Claude',workbuddy:'WorkBuddy',grok:'Grok',cursor:'Cursor',other:'MCP 客户端'};
const platformMethods={chatgpt:'ChatGPT 聊天：通过远程 MCP 接入。先准备本机网关，再配置 OpenAI 安全隧道与 ChatGPT 连接器。',codex:'Codex：为支持本机 MCP 的桌面或 CLI 入口准备连接桥和配置；具体入口的真实任务分别验证。',claude:'Claude：当前准备 Claude Code / 支持本机 MCP 的配置。Claude 网页聊天的远程连接器需要另行适配和验证。',cursor:'Cursor：准备本机 MCP 配置与添加入口，连接后可在新会话使用记忆。',workbuddy:'WorkBuddy：准备应用使用的 MCP JSON 配置；实际应用接入与自动采集分别验证。',grok:'Grok 聊天：为 grok.com 准备临时 HTTPS MCP 地址和认证。X 内嵌聊天入口另行验证。',other:'其他 AI：按该客户端支持的标准 MCP 接入。本机配置或远程方式需与客户端实际能力一致。'};
function renderPlatformSelection(){
 const host=$('connection-host').value;
 for(const button of document.querySelectorAll('[data-platform]'))button.setAttribute('aria-pressed',String(button.dataset.platform===host));
 $('connection-method').textContent=platformMethods[host]||platformMethods.other;
 $('connection-create').textContent='添加 '+(connectionHostLabels[host]||'AI')+' 连接';
 const heading=$('connections-heading');if(heading)heading.textContent=(connectionHostLabels[host]||'AI')+' 的连接';
}
for(const button of document.querySelectorAll('[data-platform]'))button.addEventListener('click',()=>{if(button.dataset.platform===$('connection-host').value)return;closeConnectionSetup();$('connection-host').value=button.dataset.platform;$('connection-host').dispatchEvent(new Event('change'));nativeSignature='';loadNative(true).catch(e=>{$('notice').textContent=e.message;});});
const connectionHostNotes={chatgpt:'为 ChatGPT 准备安全隧道和连接器配置。本机地址不会直接交给远端客户端，实际使用状态在连接后核对。',codex:'为 Codex 生成 TOML 配置。本机连接桥可用时，配置不含明文令牌。CLI 与桌面入口仍需分别验证。',claude:'Claude 聊天、Desktop 与 Code 的接法不同。支持 stdio 的本机入口可使用 Nova 连接桥；网页聊天等远端入口仍需安全连接地址，具体入口各自验证。',workbuddy:'生成 MCP JSON 配置供 WorkBuddy 验证。本机连接桥可用时配置不含明文令牌；真实保存、读取与自动采集分别验收。',grok:'目标是 grok.com 的 Grok 聊天。它需要可访问的安全连接地址；当前本地地址还需连接桥，X 内嵌入口另行验证。',cursor:'生成 MCP JSON 配置供 Cursor 使用。本机连接桥可用时配置不含明文令牌；真实 IDE 调用和可选 Hook 采集分别验收。',other:'按客户端实际支持的 MCP 方式使用连接参数。未知客户端不自动视为已兼容。'};
function showView(view){if(!['space','library','organize','connections'].includes(view))view='space';if(nativeView==='connections'&&view!=='connections')closeConnectionSetup();nativeView=view;for(const node of document.querySelectorAll('.view'))node.hidden=node.id!=='view-'+view;for(const button of document.querySelectorAll('#primary-nav button')){if(button.dataset.view===view)button.setAttribute('aria-current','page');else button.removeAttribute('aria-current');}const host=$(view==='organize'?'organize-units':'library-units');host.append($('native-units-panel'));$('native-settings-panel').hidden=true;location.hash=view;window.scrollTo(0,0);window.dispatchEvent(new CustomEvent('nova:viewchange',{detail:{view}}));}
for(const button of document.querySelectorAll('[data-view]'))button.addEventListener('click',()=>showView(button.dataset.view));
for(const button of document.querySelectorAll('[data-go]'))button.addEventListener('click',()=>showView(button.dataset.go));
window.addEventListener('hashchange',()=>{const view=location.hash.slice(1);if(view!==nativeView)showView(view);});showView(location.hash.slice(1)||'space');
$('global-capture').addEventListener('click',()=>{if(backupQuarantined||$('workspace').hidden)return;$('capture-dialog').showModal();$('content').focus();});
$('capture-close').addEventListener('click',()=>$('capture-dialog').close());
for(const id of ['native-settings','native-settings-mobile'])$(id).addEventListener('click',()=>{if($('workspace').hidden)return;$('native-settings-panel').hidden=!$('native-settings-panel').hidden;if(!$('native-settings-panel').hidden){document.querySelector('details.settings').open=true;$('native-settings-panel').scrollIntoView({block:'start'});}});
$('library-search').addEventListener('input',()=>{clearTimeout(searchTimer);searchTimer=setTimeout(async()=>{unitSignature='';try{await loadUnits();$('library-search-status').textContent=$('library-search').value?'已按整理内容筛选。':'';}catch(e){$('library-search-status').textContent='搜索尚未完成。当前搜索最多支持 128 条整理记录，请保留关键词并重试。';}},250);});
function connectionGuide(link,host){const selected=Object.hasOwn(connectionHostLabels,host)?host:'other';link.href='/install#connect-'+selected;link.textContent='查看 '+connectionHostLabels[selected]+' 接入步骤';}
function emptyReadScope(){return connectionInventory?.extraction_records===0&&$('connection-preset').value!=='capture_read'&&!$('connection-future').checked;}
function renderFirstSave(){
 const empty=!backupQuarantined&&connectionInventory?.extraction_records===0;
 $('connection-first-save').hidden=!empty;
 if(empty){
  $('connection-first-note').textContent=connectionInventory.sources>0?'材料已保存，尚未形成结构化记忆。可以继续从 AI 保存，或在 AI 整理中查看处理状态；只读连接需要已有记忆，或由你明确选择包含未来新增。':'记忆库还是空的。先连接一个 AI 保存内容，或直接在这里留住一段。保存不需要批准未来读取。';
  $('connection-save-first').textContent='先从 '+(connectionHostLabels[$('connection-host').value]||'AI')+' 保存';
 }
 const blocked=emptyReadScope();
 $('connection-create').disabled=nativeBusy||backupQuarantined||gatewayViewState?.backup_quarantined===true||blocked;
 $('connection-create').setAttribute('aria-describedby',blocked?'connection-first-note':'connection-scope-note');
}
$('connection-save-first').addEventListener('click',()=>{
 $('connection-preset').value='capture_read';$('connection-topic').value='';$('connection-future').checked=false;connectionIssueId=crypto.randomUUID();connectionFields();
 $('connection-status').textContent='已选择保存材料；新整理出的记忆不会自动开放读取。核对范围后，添加连接即可领取此平台配置。';$('connection-create').focus();
});
$('connection-capture-first').addEventListener('click',()=>{if(!backupQuarantined&&!$('workspace').hidden){$('capture-dialog').showModal();$('content').focus();}});
function connectionFields(){const topic=$('connection-preset').value==='topic';$('connection-topic').required=topic;const scoped=topic||!!$('connection-topic').value;if(scoped)$('connection-future').checked=false;$('connection-future').disabled=scoped;$('connection-scope-note').textContent=scoped?'主题连接固定当前记忆及版本。新增或修改后，请重新选择范围；自动主题归类不作为动态授权依据。':$('connection-future').checked?'允许这个库后续新增的普通私密与可分享记忆，及其后续版本。':'只允许创建连接时的这些记忆及版本；后续修改或新增需另行授权。';if($('connection-preset').value==='capture_read')$('connection-scope-note').textContent+=' 保存材料不需要授权未来读取；未勾选时，新整理出的记忆不会自动开放给此连接。';$('connection-host-note').textContent=connectionHostNotes[$('connection-host').value]||connectionHostNotes.other;connectionGuide($('connection-host-guide'),$('connection-host').value);renderFirstSave();}

for(const id of ['connection-host','connection-preset','connection-topic','connection-future'])$(id).addEventListener('change',()=>{connectionIssueId=crypto.randomUUID();connectionFields();renderPlatformSelection();});connectionFields();renderPlatformSelection();
$('connection-form').addEventListener('submit',async e=>{e.preventDefault();if(nativeBusy||backupQuarantined||emptyReadScope())return;nativeBusy=true;$('connection-create').disabled=true;$('connection-status').textContent='正在创建有范围的连接…';try{
 const host=$('connection-host').value,epoch=++setupEpoch,body={request_id:connectionIssueId,label:connectionHostLabels[host]+' · '+new Date().toLocaleDateString('zh-CN'),host,preset:$('connection-preset').value,topic_id:$('connection-topic').value||null,include_future:$('connection-future').checked};
 const result=await api('/connections',body);if(epoch===setupEpoch&&host===$('connection-host').value)presentConnectionSetup(result,host);$('connection-status').textContent='连接已创建，等待目标 AI 实测。仅授予所选范围，不包含 Vault 主钥或 Owner 凭据。';connectionIssueId=crypto.randomUUID();await loadNative(true);
 }catch(e){$('connection-status').textContent='本次连接未能确认。请查看已有连接；本机连接可从下方重新获取配置；私有文件不可用或 HTTP 令牌遗失时，再撤销重建。若只需先保存材料，可选择“采集与读取”，不必勾选未来读取。只读连接需先选择已有记忆。';await loadNative(true).catch(()=>{});}finally{nativeBusy=false;renderFirstSave();}});
function presentConnectionSetup(result,host){
 if($('workspace').hidden)return;
 issuedConfiguration=result.configuration;issuedHost=host;issuedConnectionId=result.connection.id;issuedConnectionMeta=result.connection;
 $('connection-config-heading').textContent=(connectionHostLabels[host]||'MCP 客户端')+' 接入设置';
 $('connection-configuration').hidden=false;connectionGuide($('connection-setup-guide'),host);showIssuedConfiguration(!!issuedConfiguration.local_stdio);clearTunnelInput();clearHttpsSetup();renderIssuedGateway();
 $('connection-start-prompt').textContent=result.connection.preset==='capture_read'&&!result.connection.include_future&&!result.connection.member_snapshot.length?'请把本次对话中我明确选定的内容保存到 Nova Memory，保留决定、理由、条件和未决事项，告诉我保存回执和实际整理状态。这个连接尚无可读取记忆，不要尝试读取或搜索；保存成功不代表整理完成。':result.connection.preset==='capture_read'?'请把本次对话中我明确选定的内容保存到 Nova Memory，保留决定、理由、条件和未决事项，按实际工具 schema 调用并告诉我保存回执和整理状态。':'请使用这个 Nova 连接范围内与当前任务相关的记忆，注明来源、时间和不确定项。没有可用记忆时直接说明，不要补造。';
 focusConnectionSetup(host);
}
function focusConnectionSetup(host){
 const panel=$(host==='chatgpt'?'connection-tunnel':host==='grok'?'connection-https':'connection-configuration');
 if($('workspace').hidden||$('connection-configuration').hidden||panel.hidden)return;
 panel.scrollIntoView({block:'start'});panel.focus({preventScroll:true});
}
function clearCursorInstall(){const link=$('connection-cursor-install');link.hidden=true;link.removeAttribute('href');$('connection-install-note').hidden=true;}
function showIssuedConfiguration(local){
 if(!issuedConfiguration)return;
 const config=local?issuedConfiguration.local_stdio:issuedConfiguration;
 if(!config)return;
 const gateway=!local&&issuedConfiguration.remote_gateway;
 const body=gateway?JSON.stringify({command:gateway.command,args:gateway.args},null,2):issuedHost==='codex'?config.codex_toml:JSON.stringify(local?config.mcp_json:['cursor','workbuddy'].includes(issuedHost)?config.mcp_json:config.http,null,2);
 $('connection-config-text').textContent=body;
 $('connection-config-intro').textContent=gateway?'可直接启动下方本机网关。需要手动接入时展开配置；凭据保存在本机私有文件中，配置中不含令牌。此步骤不接通外部隧道或创建公网地址。':'请仅将配置保存到你自己的客户端。本机方式通过私有文件读取凭据，HTTP 方式包含连接令牌。关闭后会清空本页配置；本机私有文件仍在且连接有效时，可以从已有连接重新获取本机配置。HTTP 令牌仅在首次创建时提供。';
 $('connection-http-config').hidden=!!gateway||!local||!issuedConfiguration.http;
 $('connection-local-config').hidden=local||!issuedConfiguration.local_stdio;
 $('connection-other-modes').hidden=$('connection-http-config').hidden&&$('connection-local-config').hidden;
 $('connection-raw-details').open=!gateway;
 $('connection-setup-note').textContent=gateway?'这是独立 MCP 网关的本机启动配置，不是可以填入远端客户端的公网地址。网关仅监听本机、要求本连接凭据，不提供工作台管理接口。'+(issuedHost==='chatgpt'?'ChatGPT 还需另行配置 Secure MCP Tunnel 的关联与认证。':'Grok 聊天还需另行配置可达的 HTTPS 隧道与认证；X 内嵌入口另验。')+' 可在本页启动、检查或停止本机网关，真实会话仍待验。退出 Nova 进程会停止托管网关；锁定页面不撤销已有连接。':local?'本机客户端配置：由目标 AI 启动 Nova 连接桥，凭据保存在本机私有文件中，配置里不含令牌。请保持当前 Nova 服务运行。此方式只供支持本机连接的入口使用；网页等远程入口仍需单独接通。真实客户端尚待验收。':connectionHostNotes[issuedHost];
 clearCursorInstall();
 if(local&&issuedHost==='cursor'&&config.contains_inline_credential===false&&config.transport==='stdio_to_loopback_http'){
  const entry=config.mcp_json?.mcpServers?.['nova-memory'];
  if(entry?.command&&Array.isArray(entry.args)){
   const encoded=btoa(String.fromCharCode(...new TextEncoder().encode(JSON.stringify(entry))));
   $('connection-cursor-install').href='cursor://anysphere.cursor-deeplink/mcp/install?'+new URLSearchParams({name:'nova-memory',config:encoded});
   $('connection-cursor-install').hidden=false;$('connection-install-note').hidden=false;
  }
 }
 $('connection-copy-status').textContent='';
}
$('connection-cursor-install').addEventListener('click',()=>{$('connection-copy-status').textContent='已请求打开 Cursor 安装确认；这不代表安装完成或记忆已使用。';});
function closeConnectionSetup(){setupEpoch++;clearHttpsSetup();$('connection-https').hidden=true;clearTunnelInput();$('connection-tunnel').hidden=true;issuedConfiguration=null;issuedHost=null;issuedConnectionId=null;issuedConnectionMeta=null;$('connection-setup-guide').removeAttribute('href');$('connection-gateway-runtime').hidden=true;$('connection-gateway-runtime').replaceChildren();clearCursorInstall();$('connection-http-config').hidden=true;$('connection-local-config').hidden=true;$('connection-config-text').textContent='';$('connection-start-prompt').textContent='';$('connection-setup-note').textContent='';$('connection-copy-status').textContent='';$('connection-configuration').hidden=true;}
$('connection-http-config').addEventListener('click',()=>showIssuedConfiguration(false));
$('connection-local-config').addEventListener('click',()=>showIssuedConfiguration(true));
$('connection-copy').addEventListener('click',async()=>{try{await navigator.clipboard.writeText($('connection-config-text').textContent);$('connection-copy-status').textContent='配置已复制，请仅粘贴到你自己的 MCP 客户端。';}catch{$('connection-copy-status').textContent='浏览器未允许复制，请使用下载配置。';}});
$('connection-download').addEventListener('click',()=>{if(!issuedConfiguration)return;const blob=new Blob([$('connection-config-text').textContent],{type:'text/plain'}),url=URL.createObjectURL(blob),a=document.createElement('a');a.href=url;a.download=issuedConfiguration.remote_gateway?'nova-memory-'+issuedHost+'-gateway.json':issuedHost==='codex'?'nova-memory-mcp.toml':'nova-memory-'+issuedHost+'.json';a.click();setTimeout(()=>URL.revokeObjectURL(url),10000);});
$('connection-prompt-copy').addEventListener('click',async()=>{try{await navigator.clipboard.writeText($('connection-start-prompt').textContent);$('connection-copy-status').textContent='开始使用的话已复制；请先在目标 AI 中完成连接。';}catch{$('connection-copy-status').textContent='浏览器未允许复制，请选中提示文字复制。';}});
$('connection-config-close').addEventListener('click',closeConnectionSetup);
function gatewayControls(connection,runtime,available,quarantined){
 const box=document.createElement('section');box.className='gateway-runtime';
 const phase=runtime?.phase||'stopped';box.dataset.gatewayPhase=phase;
 const status=text('p',phase==='running'?(runtime.upstream_verified?'本机网关已启动 · MCP 握手已确认':'本机网关已启动 · MCP 尚未确认'):phase==='stopping'?'本机网关正在停止':phase==='failed'?'本机网关已停止，启动状态未能确认':'本机网关未启动');
 status.setAttribute('role','status');box.append(status);
 if(runtime?.last_error==='authorization_unavailable')box.append(text('small','连接权限或私有配置无法确认，网关已停止接受后续请求。'));
 if(phase==='running'&&runtime.endpoint){const endpoint=text('code',runtime.endpoint);endpoint.dataset.gatewayEndpoint='';box.append(endpoint);}
 if(runtime?.checked_at)box.append(text('small','最近检查 '+new Date(runtime.checked_at).toLocaleString('zh-CN')));
 box.append(text('small','本机握手不代表平台已使用；隧道状态单独显示，公网连通与真实会话另验。'));
 const actions=document.createElement('div');actions.className='source-controls';
 const valid=!connection.revoked&&new Date(connection.expires_at)>new Date()&&!quarantined;
 function action(name,operation){
  const button=text('button',name);button.type='button';button.className='secondary';button.dataset.gatewayAction=operation;
  button.addEventListener('click',async()=>{button.disabled=true;status.textContent=operation==='start'?'正在启动并检查本机网关…':operation==='stop'?'正在停止本机网关…':'正在检查本机 MCP 握手…';try{
   const result=await api('/connections/'+encodeURIComponent(connection.id)+'/gateway/'+operation,{});
   $('notice').textContent=operation==='stop'?'网关停止已请求，连接授权仍保留；彻底断开请撤销连接。':result.gateway.upstream_verified?'本机 MCP 握手已确认，未读取或保存记忆；外部隧道与真实平台仍待验。':'本机网关状态已更新，MCP 握手尚未确认。';
   await loadNative(true);
  }catch{$('notice').textContent='本次网关操作未能确认，请刷新状态检查原连接；没有新增授权或启动外部隧道。';await loadNative(true).catch(()=>{});}finally{button.disabled=false;}});
  actions.append(button);return button;
 }
 if(valid&&available&&phase!=='running'&&phase!=='stopping')action('启动本机网关','start');
 if(phase==='running'){
  if(valid)action('检查网关','check');
  action('停止网关','stop');
  if(runtime.endpoint){const copy=text('button','复制本机转发地址');copy.type='button';copy.className='secondary';copy.addEventListener('click',async()=>{try{await navigator.clipboard.writeText(runtime.endpoint);$('notice').textContent='已复制本机网关地址，仅供隧道转发；不是远端客户端的公网地址。';}catch{$('notice').textContent='未能复制，请选中本机网关地址复制。';}});actions.append(copy);}
 }
 box.append(actions);return box;
}
function renderIssuedGateway(){
 const target=$('connection-gateway-runtime');target.replaceChildren();target.hidden=!issuedConfiguration?.remote_gateway;
 if(target.hidden||!issuedConnectionMeta)return;
 const connection=gatewayViewState?.items.find(c=>c.id===issuedConnectionId)||issuedConnectionMeta;
 const runtime=gatewayViewState?.gateway_runtimes?.find(r=>r.connection_id===issuedConnectionId);
 target.append(gatewayControls(connection,runtime,true,gatewayViewState?.backup_quarantined===true));renderTunnelSetup(connection,runtime);renderHttpsSetup(connection,runtime);
}
let httpsBusy=false,httpsPresentedEndpoint=null;
const httpsPhases={starting:'正在启动临时 HTTPS 通路',address_allocated:'已分配临时地址，等待隧道连接',connected:'隧道客户端报告已连接 · Grok 认证/会话待验',stopping:'正在停止临时通路',stopped:'临时通路已停止，旧临时凭据失效',failed:'临时通路未能保持连接，请检查本机网关和网络'};
function clearHttpsSetup(){httpsPresentedEndpoint=null;$('connection-https-consent').checked=false;$('connection-https-copy-status').textContent='';$('connection-https-live').replaceChildren();}
function httpsSummary(connection,runtime,headline=true){
 const box=document.createElement('section');box.className='gateway-runtime';box.dataset.httpsPhase=runtime?.phase||'stopped';if(headline)box.append(text('p',runtime?httpsPhases[runtime.phase]||'临时通路状态未确认':'Grok 临时 HTTPS 通路未启动'));
 if(runtime?.last_error)box.append(text('small',runtime.last_error==='authorization_or_gateway_unavailable'?'网关或连接权限失效，通路已停止。':runtime.last_error==='readiness_timeout'?'两分钟内未就绪，临时通路已停止。':'客户端已退出或诊断超出范围，请重新检查；原始日志不会显示。'));
 if(runtime?.phase==='connected'&&runtime.public_endpoint){const code=text('code',runtime.public_endpoint);code.dataset.httpsEndpoint='';box.append(code);
  const copy=text('button','复制 HTTPS MCP 地址');copy.type='button';copy.className='secondary';copy.addEventListener('click',async()=>{try{await navigator.clipboard.writeText(runtime.public_endpoint);$('notice').textContent='已复制临时地址；Grok 认证和真实会话仍待验。';}catch{$('notice').textContent='未能复制，请选中 HTTPS 地址复制。';}});box.append(copy);}
 if(runtime&&['starting','address_allocated','connected'].includes(runtime.phase)){
  const stop=text('button','停止临时 HTTPS 通路');stop.type='button';stop.className='secondary';stop.dataset.httpsStop='';stop.addEventListener('click',async()=>{stop.disabled=true;try{await api('/connections/'+encodeURIComponent(connection.id)+'/https/stop',{});$('notice').textContent='已请求停止临时通路，旧临时凭据已失效，原记忆授权与网关保留。';await loadNative(true);}catch{$('notice').textContent='停止状态未能确认，请刷新后核对。';}finally{stop.disabled=false;}});box.append(stop);
 }return box;
}
function renderHttpsSetup(connection,gateway){
 const panel=$('connection-https');panel.hidden=connection.host!=='grok';if(panel.hidden){clearHttpsSetup();return;}
 const runtime=gatewayViewState?.https_tunnel_runtimes?.find(r=>r.connection_id===connection.id);
 const endpoint=runtime?.phase==='connected'?runtime.public_endpoint:null;
 if(endpoint!==httpsPresentedEndpoint){httpsPresentedEndpoint=endpoint;$('connection-https-copy-status').textContent='';}
 $('connection-https-status').textContent=runtime?httpsPhases[runtime.phase]||'状态未确认':gatewayViewState?.https_tunnel_client_available?'本机发现 cloudflared；公网通路尚未启动。':'未发现可用 cloudflared；不会自动安装或读取现有账号配置。';
 const live=$('connection-https-live');live.replaceChildren();if(runtime)live.append(httpsSummary(connection,runtime,false));
 const active=runtime&&['starting','address_allocated','connected','stopping'].includes(runtime.phase);
 $('connection-https-form').hidden=!!active;
 $('connection-https-start').disabled=httpsBusy||!!active||gateway?.phase!=='running'||!gateway?.upstream_verified||gatewayViewState?.https_tunnel_client_available!==true;
 $('connection-https-key').disabled=!endpoint||httpsBusy;
 if(active)$('connection-https-consent').checked=false;
}
$('connection-https-form').addEventListener('submit',async event=>{
 event.preventDefault();if(httpsBusy||issuedHost!=='grok'||!issuedConnectionId||$('connection-https-start').disabled||!$('connection-https-consent').checked)return;
 const id=issuedConnectionId;httpsBusy=true;$('connection-https-consent').checked=false;$('connection-https-start').disabled=true;
 try{await api('/connections/'+encodeURIComponent(id)+'/https/start',{expose_via_cloudflare:true});$('notice').textContent='临时 HTTPS 通路启动已请求；公网地址、认证和真实 Grok 会话分别核对。';await loadNative(true);}
 catch{$('notice').textContent='本次 HTTPS 通路启动未能确认，请检查本机网关、cloudflared 与网络；没有新增记忆授权。';await loadNative(true).catch(()=>{});}
 finally{httpsBusy=false;if(issuedConnectionId===id)renderIssuedGateway();}
});
$('connection-https-key').addEventListener('click',async()=>{
 if(httpsBusy||issuedHost!=='grok'||!issuedConnectionId||!httpsPresentedEndpoint||$('workspace').hidden)return;
 const id=issuedConnectionId,endpoint=httpsPresentedEndpoint;httpsBusy=true;$('connection-https-key').disabled=true;let result;
 try{result=await api('/connections/'+encodeURIComponent(id)+'/https/configuration',{reveal_scoped_key:true});
  if($('workspace').hidden||issuedConnectionId!==id||httpsPresentedEndpoint!==endpoint||result.configuration?.url!==endpoint)throw Error('changed');
  await navigator.clipboard.writeText(result.configuration.headers.Authorization);$('connection-https-copy-status').textContent='已复制本次临时 Bearer，仅交给此 Grok 连接；停止或重开后须更新。';
 }catch{$('connection-https-copy-status').textContent='凭据未复制，请确认通路仍连接且浏览器允许复制；不会改成无认证。';}
 finally{if(result?.configuration?.headers)result.configuration.headers.Authorization='';result=null;httpsBusy=false;if(issuedConnectionId===id)renderIssuedGateway();}
});
function clearTunnelInput(){for(const id of ['connection-tunnel-id','connection-tunnel-key'])$(id).value='';$('connection-tunnel-consent').checked=false;}
const tunnelPhases={starting:'隧道客户端启动中，正在等待连接就绪',ready:'隧道客户端报告就绪 · ChatGPT 真实会话待验',stopping:'正在停止此隧道',stopped:'隧道已停止，连接授权仍保留',failed:'隧道未能保持就绪，请检查 Tunnel 关联、运行凭据与网络'};
function tunnelSummary(connection,runtime){
 const box=document.createElement('section');box.className='gateway-runtime';box.dataset.tunnelPhase=runtime?.phase||'stopped';box.append(text('p',runtime?tunnelPhases[runtime.phase]||'隧道状态未确认':'ChatGPT 隧道未启动'));
 if(runtime?.last_error)box.append(text('small',runtime.last_error==='authorization_or_gateway_unavailable'?'本机网关或连接权限失效，已停止此隧道。':runtime.last_error==='readiness_timeout'?'两分钟内未就绪，已停止；核对关联与运行凭据后重试。':'客户端已退出；不会输出原始日志或密钥，请核对客户端与配置。'));
 if(runtime&&['starting','ready'].includes(runtime.phase)){
  const stop=text('button','停止 ChatGPT 隧道');stop.type='button';stop.className='secondary';stop.dataset.tunnelStop='';stop.addEventListener('click',async()=>{stop.disabled=true;try{await api('/connections/'+encodeURIComponent(connection.id)+'/tunnel/stop',{});$('notice').textContent='已请求停止此隧道，授权与本机网关仍保留。';await loadNative(true);}catch{$('notice').textContent='停止状态尚未确认，请刷新连接状态。';}finally{stop.disabled=false;}});box.append(stop);
 }
 return box;
}
function renderTunnelSetup(connection,gateway){
 const panel=$('connection-tunnel');panel.hidden=connection.host!=='chatgpt';if(panel.hidden){clearTunnelInput();return;}
 const runtime=gatewayViewState?.tunnel_runtimes?.find(r=>r.connection_id===connection.id);
 const active=runtime&&['starting','ready','stopping'].includes(runtime.phase);
 $('connection-tunnel-status').textContent=runtime?tunnelPhases[runtime.phase]||'隧道状态未确认':gatewayViewState?.tunnel_client_available?'本机发现 Tunnel 客户端；尚未建立外部连接。':'尚未发现可用的 tunnel-client；不会自动安装或读取已有凭据。';
 $('connection-tunnel-start').disabled=!!active||gateway?.phase!=='running'||!gateway?.upstream_verified||gatewayViewState?.tunnel_client_available!==true;
 $('connection-tunnel-form').hidden=!!active;
 if(active)clearTunnelInput();
}
$('connection-tunnel-form').addEventListener('submit',async event=>{
 event.preventDefault();if(!issuedConnectionId||issuedHost!=='chatgpt'||$('connection-tunnel-start').disabled||!$('connection-tunnel-consent').checked)return;
 const id=issuedConnectionId,body={tunnel_id:$('connection-tunnel-id').value.trim(),runtime_key:$('connection-tunnel-key').value.trim(),connect_to_openai:true};
 $('connection-tunnel-key').value='';$('connection-tunnel-consent').checked=false;$('connection-tunnel-start').disabled=true;
 try{const pending=api('/connections/'+encodeURIComponent(id)+'/tunnel/start',body);body.runtime_key='';await pending;$('notice').textContent='隧道启动已请求。就绪状态不代表 ChatGPT 已关联或调用；请在实际会话验证。';await loadNative(true);}
 catch{$('notice').textContent='本次隧道启动未能确认。请检查本机网关、Tunnel ID 和独立运行密钥；密钥需重新输入。';await loadNative(true).catch(()=>{});}
 finally{body.runtime_key='';if(issuedConnectionId===id)renderIssuedGateway();}
});
function usageDescription(event){if(event.operation==='read')return '读取了 '+event.item_count+' 条记忆';return {capture:'已接收一份材料',startup_brief:'读取了启动简报',list_handoffs:'查看了任务交接列表',handoff_claim:'领取了任务交接',compaction_recap:'读取了接续摘要'}[event.operation]||'完成了一次连接操作';}
async function loadUsage(append=false){const data=await api('/usage'+(append&&usageBefore?'?before='+usageBefore:''));if(!append)$('usage').replaceChildren();for(const event of data.items){const row=document.createElement('article');row.className='record';row.append(text('p',usageDescription(event)),text('small',new Date(event.at).toLocaleString('zh-CN')));const details=document.createElement('details');details.append(text('summary','查看使用回执'),text('small',event.receipt_id),text('small','连接 '+event.connection_id));row.append(details);$('usage').append(row);}if(!append&&!data.items.length)$('usage').append(text('p','尚无使用记录。客户端实际读取或保存后会显示在这里。'));usageBefore=data.next_before;$('usage-more').hidden=!usageBefore;}
$('usage-more').addEventListener('click',()=>loadUsage(true).catch(e=>{$('notice').textContent=e.message;}));
let nativeLoading=false;
async function retrieveLocalConnectionSetup(id,host){
 const epoch=++setupEpoch;
 const current=()=>epoch===setupEpoch&&nativeView==='connections'&&!$('workspace').hidden&&host===$('connection-host').value;
 try{
  const result=await api('/connections/'+encodeURIComponent(id)+'/local-setup',{});
  if(!current())return;
  if(result.connection.id!==id||result.connection.host!==host)throw new Error('connection_changed');
  presentConnectionSetup(result,host);
  $('connection-status').textContent='已重新取得本机配置，范围、期限和额度保持原样。HTTP 令牌不会再次提供。';
 }catch{if(current())$('connection-status').textContent='无法取得本机配置。请检查这条连接是否仍有效、私有配置是否还在；需要重新接入时，可撤销旧连接后再创建。';}
}
function openTaskConnection(id,host){
 if($('workspace').hidden||!['codex','claude','cursor','workbuddy'].includes(host))return;
 closeConnectionSetup();showView('connections');
 const platform=$('connection-host');platform.value=host;platform.dispatchEvent(new Event('change'));
 $('connection-status').textContent='正在核对任务绑定的本机连接…';
 loadNative(true).catch(()=>{});
 retrieveLocalConnectionSetup(id,host);
}
function connectionDiagnostic(connection){
 const box=document.createElement('section');box.className='gateway-runtime';
 const button=text('button','检查本机连接');button.type='button';button.className='secondary';button.dataset.connectionDiagnose='';
 const status=text('p','');status.setAttribute('role','status');status.dataset.connectionDiagnostic='';status.hidden=true;
 button.addEventListener('click',async()=>{button.disabled=true;status.hidden=false;status.textContent='正在检查授权、服务与工具清单…';try{
  const result=await api('/connections/'+encodeURIComponent(connection.id)+'/diagnose',{}),check=result.diagnostic;
  const messages={ready:'本机协议与工具清单检查通过（'+check.tool_count+' 个工具）。目标 AI 是否已加载、调用，仍需在该客户端验证。',transport_unavailable:'本机服务暂时不可达，请确认 Nova 正在运行，再重新取得本机配置。',server_contract_mismatch:'连接到的服务协议不匹配，请使用当前 Nova 生成的配置。',tool_schema_invalid:'工具清单或格式不完整，客户端可能无法加载。请更新 Nova，并在客户端刷新工具。',timeout:'本机检查超时，请稍后重试。'};
  status.textContent=messages[check.status]||'本机检查未能确认，请重新取得连接配置后重试。';
  if(result.bridge_available===false)status.textContent+=' 当前未发现本机连接程序；本项没有验证 stdio 程序启动。';
  status.append(text('small','本次没有读取、保存记忆或调用模型；没有新增授权，也没有启动连接程序、目标 AI 或外部隧道。'));
 }catch{status.textContent='连接或私有配置无法确认，请检查是否已到期或撤销，再重新取得原连接配置。';}finally{button.disabled=false;}});
 box.append(button,status);return box;
}
async function loadNative(force=false){if(nativeLoading||$('workspace').hidden)return;nativeLoading=true;try{
 const [inventory,connections,wikis]=await Promise.all([api('/overview'),api('/connections'),api('/wiki')]);
 connectionInventory=inventory;gatewayViewState=connections;renderFirstSave();const signature=JSON.stringify([inventory,connections,wikis]);if(!force&&signature===nativeSignature)return;nativeSignature=signature;
 $('stat-sources').textContent=inventory.sources;$('stat-units').textContent=inventory.extraction_records;$('stat-wiki').textContent=inventory.wiki_pages;
 $('space-recent').replaceChildren();for(const wiki of wikis.items.slice(0,3)){const card=document.createElement('article');card.className='record';card.append(text('h3',({user:'关于我',self:'关于我'})[wiki.subject]||wiki.subject));for(const memory of wiki.memories.slice(0,2)){card.append(text('p',valueText(memory.content.value)));card.append(text('small',(memory.content.polarity==='negative'?'否定陈述':'肯定陈述')+' · '+({asserted:'陈述',planned:'计划',hypothetical:'假设',unknown:'尚不确定'})[memory.content.modality]+' · '+({user_statement:'用户自述',extracted:'AI 提取',inferred:'AI 推断',external_claim:'外部材料陈述',agent_experience:'Agent 经历记录'})[memory.origin]+' · 语义尚未独立验证'));}card.append(text('small',wiki.memories.length+' 条理解 · '+new Date(wiki.updated_at).toLocaleString('zh-CN')));$('space-recent').append(card);}if(!wikis.items.length)$('space-recent').append(text('p',inventory.consolidation.pending?'新材料正在整理。完成后，带来源的理解会显示在这里。':inventory.consolidation.cold?'主题缓存已清理，记忆仍在。可在 AI 整理的本地维护中重新生成。':'还没有形成主题理解。先留住一段值得记住的内容。'));
 const selected=$('connection-topic').value,selectedLabel=$('connection-topic').selectedOptions[0]?.textContent;$('connection-topic').replaceChildren(new Option('当前全部记忆',''));for(const wiki of wikis.items)$('connection-topic').add(new Option((({user:'关于我',self:'关于我'})[wiki.subject]||wiki.subject)+' · '+valueText(wiki.memories[0]?.content.value).slice(0,32)+'（'+wiki.memories.length+' 条）',wiki.id));if(selected&&![...$('connection-topic').options].some(o=>o.value===selected))$('connection-topic').add(new Option(selectedLabel||'主题正在更新（保留当前选择）',selected));$('connection-topic').value=selected;
 renderFirstSave();
 $('connection-health').textContent=connections.backup_quarantined?'此备份副本尚未核对后续删除与撤权，AI 连接保持关闭。':connections.service_ready?'本机连接服务已就绪。配置完成后，请在所选 AI 中测试使用。':'本机连接服务未就绪。';
 if(issuedConnectionId&&(connections.backup_quarantined||connections.items.some(c=>c.id===issuedConnectionId&&(c.revoked||new Date(c.expires_at)<=new Date()))))closeConnectionSetup();
 renderIssuedGateway();
 renderPlatformSelection();for(const label of document.querySelectorAll('[data-platform-status]')){const rows=connections.items.filter(c=>c.host===label.dataset.platformStatus&&!c.revoked&&new Date(c.expires_at)>new Date());label.textContent=rows.length?rows.length+' 个连接配置':'尚未添加连接';}
 const platformConnections=connections.items.filter(c=>c.host===$('connection-host').value);
 $('connections').replaceChildren();for(const connection of platformConnections){const row=document.createElement('article');row.className='record';row.dataset.connectionId=connection.id;const expired=new Date(connection.expires_at)<=new Date();row.append(text('h3',connection.label),text('p',presetLabels[connection.preset]+' · '+(connection.revoked?'已撤销':expired?'已到期':'授权有效')),text('small','创建时 '+connection.member_snapshot.length+' 条记录'+(connection.include_future?' · 含所选范围后续新增与版本':' · 仅这些记忆及版本')),text('small',connection.last_used_at?'最近使用 '+new Date(connection.last_used_at).toLocaleString('zh-CN'):'等待客户端首次使用'),text('small','已使用 '+connection.used_calls+' / '+connection.call_budget+' 次 · 到期 '+new Date(connection.expires_at).toLocaleString('zh-CN')));
 if(!connection.revoked&&!expired&&connection.preset==='capture_read'&&!connection.include_future&&!connection.member_snapshot.length)row.append(text('p','此连接可保存材料，尚无可读取记忆。新整理内容不会自动开放；准备使用时，请重新选择读取范围并添加连接。'));
 if(['chatgpt','grok'].includes(connection.host))row.append(gatewayControls(connection,connections.gateway_runtimes?.find(r=>r.connection_id===connection.id),connections.local_bridge_ready,connections.backup_quarantined));
 if(connection.host==='grok')row.append(httpsSummary(connection,connections.https_tunnel_runtimes?.find(r=>r.connection_id===connection.id)));
 if(connection.host==='chatgpt')row.append(tunnelSummary(connection,connections.tunnel_runtimes?.find(r=>r.connection_id===connection.id)));
 if(!connection.revoked&&!expired){
 if(connections.local_bridge_ready&&['codex','claude','cursor','workbuddy'].includes(connection.host)){
  const setup=text('button','重新获取本机配置');setup.type='button';setup.className='secondary';
  setup.addEventListener('click',async()=>{setup.disabled=true;try{await retrieveLocalConnectionSetup(connection.id,connection.host);}finally{setup.disabled=false;}});row.append(setup);
 }
 if(connections.local_bridge_ready&&['chatgpt','grok'].includes(connection.host)){
  const setup=text('button','连接 '+connectionHostLabels[connection.host]);setup.type='button';setup.className='secondary';setup.dataset.remoteSetup=connection.host;setup.setAttribute('aria-controls',connection.host==='chatgpt'?'connection-tunnel':'connection-https');
  setup.addEventListener('click',async()=>{setup.disabled=true;try{
   const epoch=++setupEpoch;const result=await api('/connections/'+encodeURIComponent(connection.id)+'/gateway-setup',{});if(epoch!==setupEpoch||connection.host!==$('connection-host').value)return;
   presentConnectionSetup(result,connection.host);$('connection-status').textContent=connection.host==='chatgpt'?'ChatGPT 接入区已打开。可在“连接 ChatGPT 安全隧道”中填写 Tunnel ID 和运行密钥；打开此区域不会启停隧道或改变授权。':'Grok 接入区已打开。可在这里管理临时 HTTPS 地址和认证；打开此区域不会启停公网通路或改变授权。';
  }catch{$('notice').textContent='无法取得网关配置：请检查原连接与本机私有文件；未启动隧道或新增授权。';}finally{setup.disabled=false;}});row.append(setup);
  row.append(text('small',connection.host==='chatgpt'?'打开 Tunnel ID 与运行密钥输入区；点击此处不会启动隧道。':'打开临时 HTTPS 地址与认证配置；点击此处不会开启公网入口。'));
 }
 if(!expired&&!connections.backup_quarantined)row.append(connectionDiagnostic(connection));
 const revoke=text('button','撤销连接');revoke.className='secondary';revoke.type='button';revoke.addEventListener('click',async()=>{revoke.disabled=true;try{const result=await api('/connections/'+encodeURIComponent(connection.id)+'/revoke',{});if(issuedConnectionId===connection.id)closeConnectionSetup();if(result.local_setup_removed===false)$('notice').textContent='连接已撤销，本机连接配置未能自动清理。后续读取仍会被拒绝，请在本机检查配置文件。';await loadNative(true);}catch(e){$('notice').textContent=e.message;revoke.disabled=false;}});row.append(revoke);
 }else{
  row.append(text('small','此连接已'+(expired?'到期':'撤销')+'，接入输入区已关闭。重新使用需要创建新连接，系统不会自动续期。'));
  const reconnect=text('button','新建 '+(connectionHostLabels[connection.host]||'MCP 客户端')+' 连接');reconnect.type='button';reconnect.className='secondary';reconnect.addEventListener('click',()=>{closeConnectionSetup();showView('connections');$('connection-host').value=connection.host;connectionIssueId=crypto.randomUUID();connectionFields();$('connection-form').scrollIntoView({block:'start'});$('connection-host').focus({preventScroll:true});$('connection-status').textContent='请重新核对范围后创建连接；旧连接不会恢复或自动续期。';});row.append(reconnect);
 }$('connections').append(row);}if(!platformConnections.length)$('connections').append(text('p','尚未添加这个平台的连接。选择使用范围后，Nova 会准备对应配置。'));await loadUsage();
 }finally{nativeLoading=false;}}
new MutationObserver(()=>{const active=!$('workspace').hidden;if(active&&!nativeActive){nativeActive=true;loadNative(true).catch(e=>{$('notice').textContent=e.message;});}else if(!active){nativeActive=false;closeConnectionSetup();}}).observe($('workspace'),{attributes:true,attributeFilter:['hidden']});
setInterval(()=>{if(!document.hidden&&!$('workspace').hidden)loadNative().catch(e=>{$('notice').textContent=e.message;});},2500);

$('connection-refresh').addEventListener('click',async()=>{$('connection-refresh').disabled=true;try{await loadNative(true);}catch(e){$('connection-health').textContent='本机服务检查未完成，请保留当前配置并重试。';}finally{$('connection-refresh').disabled=false;}});
