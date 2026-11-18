'use strict';
// Owner-only task projection. No storage, client request, model call or implicit claim.
(()=>{
 const panel=$('task-continuity'),cards=$('task-cards'),status=$('task-status'),dialog=$('task-resume-dialog');
 let epoch=0,before=null,next=null,signature='',loading=false,acting=false,prepared=null;
 const active=()=>!document.hidden&&!$('workspace').hidden&&!$('view-space').hidden;
 const current=e=>e===epoch&&active();
 const hostName=h=>({codex:'Codex',claude:'Claude Code',cursor:'Cursor',workbuddy:'WorkBuddy',chatgpt:'ChatGPT',grok:'Grok'}[h]||'MCP 客户端');
 function forget(){prepared=null;$('task-resume-request').value='';$('task-resume-note').textContent='';$('task-resume-status').textContent='';if(dialog.open)dialog.close();}
 function clear(){epoch++;forget();cards.replaceChildren();signature='';status.textContent='';panel.hidden=true;}
 function buttons(){for(const b of cards.querySelectorAll('button'))b.disabled=acting||b.dataset.unsupported==='true';$('task-refresh').disabled=acting||loading;$('task-more').disabled=acting||loading;$('task-first').disabled=acting||loading;}
 function group(row,title,values){if(!values.length)return;const box=document.createElement('div');box.append(text('h4',title));const list=document.createElement('ul');for(const value of values)list.append(text('li',value));box.append(list);row.append(box);}
 function render(data){
  cards.replaceChildren();next=data.next_before;$('task-first').hidden=before===null;$('task-more').hidden=next===null;
  for(const item of data.items){const c=item.card,e=c.envelope,m=e.metadata,row=document.createElement('article');row.className='task-card';
   row.append(text('p',hostName(c.recipient_host)+' · '+c.recipient_label),text('h3',e.content.summary),text('small',m.state==='claimed'?'已领取交接 · 实际使用未确认':'待接续 · 任务资料未经独立验证'));
   const notes=e.owner_notes||[];if(notes.length){const latest=notes[notes.length-1];row.append(text('small',latest.kind==='user_task_goal'?'你写下的任务目标':'最新用户补充 · 尚未由 AI 重新整理'),text('p',latest.text));}if(e.supersedes)row.append(text('small','已替换上一份交接，旧接续请求已失效。'));if(!e.content.next_steps.length)row.append(text('small','尚未生成下一步。接续时由 AI 结合依据与任务说明处理。'));group(row,notes.some(n=>n.kind==='user_task_clarification')?'原计划（请结合补充核对）':'下一步',e.content.next_steps);const groups=document.createElement('details');groups.className='task-groups';groups.append(text('summary','进展、决定与约束'));for(const [key,title] of [['completed','已完成的记录'],['open_tasks','尚待处理'],['decisions','已有决定'],['failed_attempts','尝试与失败'],['constraints','约束']])group(groups,title,e.content[key]);row.append(groups);
   const details=document.createElement('details');details.append(text('summary','查看依据与有效期'),text('p','到期 '+new Date(m.expires_at).toLocaleString('zh-CN')),text('p','以下是交接绑定的精确版本；内容更新后需重新生成交接。'));for(const ref of e.memories)details.append(text('small',ref.object_id+' · v'+ref.revision));details.append(text('small','交接版本 '+m.revision+' · '+m.handoff_id));row.append(details);
   const actions=document.createElement('div');actions.className='source-controls';const resume=text('button','继续这个任务');resume.type='button';resume.dataset.unsupported=String(!item.resume_supported);resume.addEventListener('click',()=>prepare(c));const cancel=text('button','结束这份交接');cancel.type='button';cancel.className='secondary';cancel.addEventListener('click',()=>cancelTask(c));const amend=text('button','补充或纠正');amend.type='button';amend.className='secondary';amend.addEventListener('click',()=>window.dispatchEvent(new CustomEvent('nova:task-edit',{detail:{card:c}})));actions.append(resume,amend,cancel);row.append(actions);
   row.append(text('small',item.resume_supported?'需要对应客户端的本机 MCP 连接。点击只准备请求，尚未调用 AI。':'该平台的接续传输尚未接通，当前仅展示任务。'));cards.append(row);
  }
  if(!data.items.length)cards.append(text('p',next!==null?'本页任务已失效或不可接续，可翻页查看更早记录。':'本页暂无可接续任务。已有、带依据的任务交接会显示在这里；普通采集材料不会被自动冒充为任务进展。'));
  buttons();
 }
 async function refresh(){if(!active()||loading)return;loading=true;buttons();const e=epoch;try{
  const service=await api('/connections');if(!current(e))return;
  if(service.capabilities.continuity!==true||service.backup_quarantined){clear();return;}
  const data=await api('/continuity/tasks'+(before===null?'':'?before='+before));if(!current(e))return;
  const fresh=JSON.stringify(data);if(fresh!==signature){forget();signature=fresh;render(data);}panel.hidden=false;
 }catch{if(current(e)){forget();cards.replaceChildren();signature='';status.textContent='任务暂时不可用。请刷新，核对连接、会话和依据仍然有效。';}}finally{loading=false;buttons();}}
 async function copy(){const p=prepared;if(!p||!current(p.epoch)||Date.parse(p.expires_at)<=Date.now()){forget();return;}
  try{await navigator.clipboard.writeText(p.request);if(prepared===p&&current(p.epoch))$('task-resume-status').textContent='已复制。请粘贴到 '+hostName(p.host)+' 的新对话中。';}
  catch{if(prepared===p&&current(p.epoch)){$('task-resume-status').textContent='浏览器未允许复制，请选中下方接续请求手动复制。';$('task-resume-request').focus();$('task-resume-request').select();}}
 }
 async function prepare(c){if(acting||!active())return;acting=true;forget();buttons();const e=epoch,m=c.envelope.metadata;try{
  const data=await api('/continuity/tasks/'+encodeURIComponent(m.handoff_id)+'/prepare-resume',{expected_revision:m.revision,recipient:m.recipient});if(!current(e))return;
  const ref=data.reference;
  if(Date.parse(ref.expires_at)<=Date.now())throw new Error('expired');
  prepared={epoch:e,expires_at:ref.expires_at,request:data.request,host:ref.recipient_host,connectionId:ref.recipient.connection_id};
  $('task-resume-request').value=data.request;$('task-resume-note').textContent='请使用 '+hostName(ref.recipient_host)+' 中的连接“'+ref.recipient_label+'”。到期 '+new Date(ref.expires_at).toLocaleString('zh-CN')+'。';
  dialog.showModal();$('task-resume-copy').focus();
 }catch{if(current(e)){forget();cards.replaceChildren();signature='';status.textContent='这份交接已改变或不可继续。请刷新任务后重新选择。';}}finally{acting=false;buttons();}}
 async function cancelTask(c){if(acting||!active())return;acting=true;buttons();try{await api('/continuity/handoffs/'+encodeURIComponent(c.envelope.metadata.handoff_id)+'/cancel',{});}catch{if(active())status.textContent='结束状态尚未确认，请刷新核对。';}finally{acting=false;buttons();await refresh();}}
 $('task-refresh').addEventListener('click',()=>{forget();refresh();});
 for(const [id,select] of [['task-first',()=>null],['task-more',()=>next]])$(id).addEventListener('click',()=>{const cursor=select();clear();before=cursor;refresh();});
 $('task-resume-close').addEventListener('click',forget);dialog.addEventListener('cancel',forget);$('task-resume-copy').addEventListener('click',copy);
 $('task-resume-connection').addEventListener('click',()=>{
  const p=prepared;if(!p||!current(p.epoch)||Date.parse(p.expires_at)<=Date.now()){forget();return;}
  forget();openTaskConnection(p.connectionId,p.host);
 });
 window.addEventListener('nova:viewchange',()=>{clear();before=null;refresh();});
 window.addEventListener('nova:intake-mutation',e=>{if(!/\/prepare-resume$/.test(e.detail?.path||'')){clear();before=null;refresh();}});
 new MutationObserver(()=>{if($('workspace').hidden)clear();else refresh();}).observe($('workspace'),{attributes:true,attributeFilter:['hidden']});
 document.addEventListener('visibilitychange',()=>{clear();if(active())refresh();});window.addEventListener('pagehide',clear);
 setInterval(()=>{if(prepared&&Date.parse(prepared.expires_at)<=Date.now())forget();if(active()&&!acting)refresh();},3000);
 refresh();
})();
