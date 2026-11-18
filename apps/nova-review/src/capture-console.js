'use strict';
// Capture credentials stay in page memory until the owner downloads the setup file.
// No localStorage, automatic install, transcript discovery or memory read grant.
(()=>{
 const panel=$('automatic-capture'),status=$('auto-capture-status'),list=$('capture-grants');
 let loading=false,busy=false,setup=null,issueId=crypto.randomUUID(),enabled=false;
 const hostNames={codex:'Codex',claude:'Claude Code'};
 function clearSetup(){setup=null;$('capture-setup').hidden=true;}
 function description(){const claude=$('auto-capture-host').value==='claude';$('auto-capture-scope').textContent=claude?'接收当前会话的可见 AI 回复。当前适配不会把 Claude 的自动化输入当作你本人说的话。':'接收当前会话的可见输入与 AI 回复。仅保留事件提供的文本；来源角色不等于事实已由你确认。';issueId=crypto.randomUUID();}
 async function refresh(){
  if(loading||$('workspace').hidden||panel.closest('.view').hidden)return;
  loading=true;
  try{
   const service=await api('/connections');enabled=service.capabilities.capture_ingress===true;
   $('auto-capture-create').disabled=!enabled||busy;
   $('capture-disabled').hidden=enabled;
   $('capture-disabled').textContent=service.backup_quarantined?'当前备份副本保持隔离，自动采集关闭。':'本机服务尚未启用自动采集入口。手动保存与已有 MCP 连接可继续使用。';
   if(!enabled){list.replaceChildren();return;}
   const response=await api('/capture-grants');list.replaceChildren();
   for(const grant of response.items){
    const row=document.createElement('article');row.className='record';row.dataset.captureGrant=grant.grant_id;
    const expired=new Date(grant.expires_at)<=new Date(),live=!grant.revoked&&!expired;
    row.append(text('h3',hostNames[grant.host]||grant.host));
    row.append(text('p',grant.revoked?'已撤销':expired?'已到期':grant.received_events?'已有接收记录 · 已保存 '+grant.received_events+' 条事件':'授权已准备 · 等待首次接收'));
    row.append(text('small','已保存 '+grant.received_events+' / '+grant.event_budget+' 条 · 到期 '+new Date(grant.expires_at).toLocaleString('zh-CN')));
    row.append(text('small','仅采集 · 客户端安装与兼容性待实际验证'));
    if(live){
     const revoke=text('button','撤销自动采集');revoke.type='button';revoke.className='secondary';
     revoke.addEventListener('click',async()=>{revoke.disabled=true;try{await api('/capture-grants/'+encodeURIComponent(grant.grant_id)+'/revoke',{});if(setup?.grant.grant_id===grant.grant_id)clearSetup();status.textContent='已撤销。之后的事件会被拒绝；已经保存的材料仍可在记忆库管理。';await refresh();}catch{status.textContent='尚未确认撤销，请保留当前页面并重试。';revoke.disabled=false;}});
     row.append(revoke);
    }
    list.append(row);
   }
   if(!response.items.length)list.append(text('p','尚未启用自动采集。你也可以继续手动留住材料。'));
  }catch{status.textContent='自动采集状态暂时不可用，现有授权状态未作更改。';}
  finally{loading=false;}
 }
 $('auto-capture-host').addEventListener('change',description);description();
 $('auto-capture-form').addEventListener('submit',async event=>{
  event.preventDefault();if(busy||!enabled)return;busy=true;$('auto-capture-create').disabled=true;
  const host=$('auto-capture-host').value;status.textContent='正在准备仅用于采集的配置…';
  try{
   const issued=await api('/capture-grants',{request_id:issueId,source_namespace:'nova-capture-'+host,host,surface:'cli_hooks',host_version:'unverified',events:host==='claude'?['assistant_response']:['user_prompt','assistant_response'],lifetime_seconds:86400,event_budget:1000,byte_budget:8*1024*1024});
   setup={schema_version:'nova.capture.setup.v0.1',endpoint:issued.endpoint,credential:issued.credential,grant:issued.grant};
   $('capture-setup').hidden=false;$('capture-setup-host').textContent=hostNames[host];
   $('capture-download-status').textContent='';status.textContent='配置已准备。下载后由本机采集程序安装；此时还没有采集任何会话。';issueId=crypto.randomUUID();
   await refresh();
  }catch{status.textContent='未能确认创建。请查看已有采集授权；同一客户端已有有效授权时，请先撤销后重新配置。';await refresh();}
  finally{busy=false;$('auto-capture-create').disabled=!enabled;}
 });
 $('capture-setup-download').addEventListener('click',()=>{
  if(!setup)return;
  const url=URL.createObjectURL(new Blob([JSON.stringify(setup,null,2)+'\n'],{type:'application/json'})),a=document.createElement('a');
  a.href=url;a.download='nova-capture-'+setup.grant.host+'.json';a.click();setTimeout(()=>URL.revokeObjectURL(url),1000);
  $('capture-download-status').textContent='配置下载已发起。文件含本次采集凭据，请只交给自己的本机采集程序。';
 });
 $('capture-setup-close').addEventListener('click',clearSetup);
 $('auto-capture-refresh').addEventListener('click',refresh);
 window.addEventListener('nova:viewchange',refresh);
 new MutationObserver(()=>{if(!$('workspace').hidden)refresh();else clearSetup();}).observe($('workspace'),{attributes:true,attributeFilter:['hidden']});
 window.addEventListener('pagehide',clearSetup);
 setInterval(()=>{if(!document.hidden)refresh();},5000);
})();
