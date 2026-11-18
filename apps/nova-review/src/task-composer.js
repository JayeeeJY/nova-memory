'use strict';
// Explicit owner goal/clarification only. A saved sentence is not an AI interpretation.
(()=>{
 const dialog=$('task-editor-dialog'),form=$('task-editor-form'),instruction=$('task-editor-instruction'),select=$('task-editor-connection'),status=$('task-editor-status');
 let epoch=0,state=null,busy=false,loading=false,request=crypto.randomUUID(),ownMutation=null;
 const active=()=>dialog.open&&!document.hidden&&!$('workspace').hidden;
 const current=e=>e===epoch&&active();
 const hostName=h=>({codex:'Codex',claude:'Claude Code',cursor:'Cursor',workbuddy:'WorkBuddy'}[h]||'MCP 客户端');
 function controls(){select.disabled=busy||loading;instruction.disabled=busy;$('task-editor-submit').disabled=busy||loading||!state||!instruction.value.trim()||state.mode==='create'&&!select.value;}
 function clear(){epoch++;state=null;busy=false;loading=false;ownMutation=null;request=crypto.randomUUID();instruction.value='';select.replaceChildren();status.textContent='';$('task-editor-background').textContent='';if(dialog.open)dialog.close();controls();}
 async function options(e){if(!current(e)||state.mode!=='create'||loading||busy)return;loading=true;controls();const selected=select.value;
  try{const memory=state.memory,data=await api('/continuity/task-options?memory_id='+encodeURIComponent(memory.object_id)+'&revision='+memory.revision);if(!current(e))return;select.replaceChildren(new Option('请选择已有连接',''));for(const item of data.items)select.add(new Option(hostName(item.host)+' · '+item.label,item.id));if(data.items.some(i=>i.id===selected))select.value=selected;else if(!state.optionsSeen&&data.items.length===1)select.value=data.items[0].id;else select.value='';state.optionsSeen=true;status.textContent=data.items.length?'只列出已获准读取这条记忆的本机连接。':'还没有可用连接。请先在“连接”中为相应 AI 添加包含这条记忆的连接。';}
  catch{if(current(e)){select.replaceChildren();status.textContent='连接或记忆已改变，暂时不能创建任务。你的说明仍保留在当前弹窗中。';}}
  finally{if(e===epoch){loading=false;controls();}}
 }
 function open(detail,mode){if($('workspace').hidden||document.hidden)return;clear();state=mode==='create'?{mode,memory:{...detail.memory}}:{mode,card:detail.card};$('task-editor-heading').textContent=mode==='create'?'从记忆开始任务':'补充或纠正任务';$('task-editor-label').textContent=mode==='create'?'接下来想做什么？':'想补充什么，或哪里需要调整？';instruction.placeholder=mode==='create'?'用一句话写下任务目标…':'例如：这个安排只适用于测试环境，下一步先核对时区。';$('task-editor-background').textContent=mode==='create'?'选中的记忆：'+detail.title:detail.card.envelope.content.summary;$('task-editor-connection-field').hidden=mode!=='create';select.required=mode==='create';$('task-editor-boundary').textContent=mode==='create'?'只保存你的目标，内部会话由 Nova 准备。不调用模型或扩大连接权限。':'逐字保留你的补充并形成新交接，旧接续请求随之失效。原计划保留供核对，不代表 AI 已完成重新整理；有效期保持不变。';$('task-editor-submit').textContent=mode==='create'?'保存任务':'保存补充并更新交接';dialog.showModal();controls();if(mode==='create')options(epoch);instruction.focus();}
 window.addEventListener('nova:task-open',e=>{if(e.detail?.memory&&typeof e.detail.title==='string')open(e.detail,'create');});
 window.addEventListener('nova:task-edit',e=>{if(e.detail?.card?.envelope)open(e.detail,'amend');});
 instruction.addEventListener('input',()=>{request=crypto.randomUUID();controls();});select.addEventListener('change',()=>{request=crypto.randomUUID();controls();});
 form.addEventListener('submit',async event=>{event.preventDefault();if(busy||loading||!active()||!state)return;if(new TextEncoder().encode(instruction.value).length>1024){status.textContent='这段说明较长，请精简为一句话。';return;}busy=true;controls();const e=epoch,selected=state;
  try{let path,body;if(selected.mode==='create'){if(!select.value)throw new Error('no_connection');path='/continuity/tasks';body={request_id:request,memory:selected.memory,connection_id:select.value,instruction:instruction.value};}else{const m=selected.card.envelope.metadata;path='/continuity/tasks/'+encodeURIComponent(m.handoff_id)+'/amend';body={request_id:request,expected_revision:m.revision,recipient:m.recipient,instruction:instruction.value};}ownMutation=path;await api(path,body);if(!current(e))return;clear();showView('space');}
  catch{if(current(e))status.textContent='尚未确认保存。请核对连接、记忆或任务是否已更新。说明仍保留；原样重试不会重复创建。';}
  finally{if(e===epoch){ownMutation=null;busy=false;controls();}}
 });
 $('task-editor-close').addEventListener('click',clear);dialog.addEventListener('cancel',event=>{event.preventDefault();clear();});
 window.addEventListener('nova:viewchange',clear);
 window.addEventListener('nova:intake-mutation',e=>{const path=e.detail?.path||'';if(path!==ownMutation&&!path.endsWith('/prepare-resume'))clear();});
 new MutationObserver(()=>{if($('workspace').hidden)clear();}).observe($('workspace'),{attributes:true,attributeFilter:['hidden']});
 document.addEventListener('visibilitychange',()=>{if(document.hidden)clear();});window.addEventListener('pagehide',clear);
 async function recheckEdit(){if(!active()||state?.mode!=='amend'||busy||loading)return;const e=epoch,m=state.card.envelope.metadata;loading=true;controls();try{const data=await api('/continuity/tasks/'+encodeURIComponent(m.handoff_id)+'/edit-state');if(!current(e))return;if(data.handoff.revision!==m.revision||data.handoff.recipient.session_id!==m.recipient.session_id)throw new Error('changed');}catch{if(current(e)){clear();$('task-status').textContent='这份任务已改变或失效，编辑已关闭。请刷新后重新选择。';}}finally{if(e===epoch){loading=false;controls();}}}
 setInterval(()=>{if(active()){if(state?.mode==='create')options(epoch);else recheckEdit();}},3000);
})();
