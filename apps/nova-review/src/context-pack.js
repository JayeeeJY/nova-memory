'use strict';
(()=>{
 const dialog=$('context-pack-dialog'),selection=new Map();let cursor=null,preview=null,epoch=0,operation=0,busy=false;
 const validState=()=>dialog.open&&!$('workspace').hidden&&!backupQuarantined;
 function invalidate(){epoch++;preview=null;$('context-pack-preview').hidden=true;$('context-pack-text').value='';$('context-pack-meta').textContent='';$('context-pack-summary').replaceChildren();$('context-pack-raw').open=false;controls();}
 function controls(){
  $('context-pack-count').textContent='已选 '+selection.size+' / 16 条记忆';
  $('context-pack-prepare').disabled=busy||!selection.size||!validState();
  $('context-pack-prepare').textContent=preview?'重新预览选定内容':'预览选定上下文';
  $('context-pack-copy').disabled=busy||!preview||!validState();$('context-pack-download').disabled=busy||!preview||!validState();
  $('context-pack-more').disabled=busy;$('context-pack-refresh').disabled=busy;
 }
 function clear(){operation++;busy=false;invalidate();selection.clear();cursor=null;$('context-pack-list').replaceChildren();$('context-pack-status').textContent='';$('context-pack-more').hidden=true;controls();}
 function close(){dialog.close();clear();}
 function brief(value){const t=typeof value==='string'?value:JSON.stringify(value);return t.length>240?t.slice(0,240)+'…':t;}
 function title(content){return (({user:'关于我',self:'关于我'})[content.subject]||content.subject)+' · '+content.predicate.split(':').pop().replaceAll('_',' ')+'：'+brief(content.value);}
 function conditions(content){return Object.entries(content.conditions).map(([k,v])=>(({'scope:context':'场景','scope:exception':'例外','scope:comparison':'比较条件'})[k]||k.split(':').pop().replaceAll('_',' '))+'：'+brief(v)).join('；');}
 function renderSummary(pack){
  const host=$('context-pack-summary');host.replaceChildren();
  for(const m of pack.payload.memories){const row=document.createElement('article');row.className='context-pack-memory';row.append(text('p',title(m.content)),text('small',(m.content.polarity==='negative'?'否定陈述 · ':'')+({asserted:'陈述',planned:'计划',hypothetical:'假设',unknown:'不确定'})[m.content.modality]+' · 版本 '+m.memory.revision+(m.protected?' · 保留你的纠正':'')));
   if(Object.keys(m.content.conditions).length)row.append(text('p',conditions(m.content)));
   const roles=[...new Set(m.sources.map(s=>({user:'用户',assistant:'AI 对话',mixed:'混合来源',external:'外部材料',agent:'Agent'})[s.role]||s.role))];
   row.append(text('small','来源：'+roles.join('、')+' · '+(m.valid_time.kind==='unknown'?'适用时间未说明':m.valid_time.start+' 至 '+(m.valid_time.end||'未说明结束'))+' · 尚未独立核实'));host.append(row);
  }
 }
 async function load(append=false){
  if(!validState()||busy)return;busy=true;controls();const run=epoch,task=++operation;
  try{
   const data=await api('/units'+(append&&cursor?'?before='+encodeURIComponent(cursor):''));if(run!==epoch||!validState())return;
   if(!append)$('context-pack-list').replaceChildren();let count=0;
   for(const item of data.items){const u=item.memory.unit;
    if(u.header.state!=='active'||!['private','shareable'].includes(u.header.privacy_level))continue;
    count++;const id=u.header.object.object_id,reference=u.header.object;
    const label=document.createElement('label');label.className='context-pack-choice';label.dataset.contextMemory=id;
    const check=document.createElement('input');check.type='checkbox';check.dataset.contextSelect='';check.checked=selection.get(id)?.revision===reference.revision;
    if(selection.has(id)&&!check.checked){selection.delete(id);invalidate();$('context-pack-status').textContent='记忆已有新版本，请查看后重新选择。';}
    const body=document.createElement('span');body.append(text('span',title(u.content)),text('small','版本 '+reference.revision+' · '+(item.memory.protected?'保留你的纠正 · ':'')+'AI 陈述尚未独立核实'));
    if(u.content.polarity==='negative')body.append(text('small','否定陈述'));
    if(Object.keys(u.content.conditions).length)body.append(text('small',conditions(u.content)));
    check.addEventListener('change',()=>{invalidate();$('context-pack-status').textContent='';if(check.checked){if(selection.size>=16){check.checked=false;$('context-pack-status').textContent='每份最多 16 条，请先取消一条再选择。';}else selection.set(id,{object_id:id,revision:reference.revision});}else selection.delete(id);controls();});
    label.append(check,body);$('context-pack-list').append(label);
   }
   cursor=data.next_before;$('context-pack-more').hidden=!cursor;
   if(!append&&!count)$('context-pack-list').append(text('p',cursor?'当前页没有可用于上下文包的有效记忆，可查看下一页。':'还没有可选的整理记忆。先保存材料，待整理完成后再来。'));
  }catch{if(run===epoch&&validState())$('context-pack-status').textContent='未能读取可选记忆，请刷新后重试。';}
  finally{if(task===operation)busy=false;controls();}
 }
 function open(){
  if($('workspace').hidden||backupQuarantined){$('notice').textContent='请先打开可用的记忆库。';return;}
  clear();$('context-pack-target').value=typeof issuedHost!=='undefined'&&issuedHost?issuedHost:'cursor';dialog.showModal();controls();return true;
 }
 for(const button of document.querySelectorAll('[data-context-pack-open]'))button.addEventListener('click',()=>{if(open())load();});
 window.addEventListener('nova:context-pack-open',event=>{
  const reference=event.detail?.reference;
  if(!reference||typeof reference.object_id!=='string'||!/^[A-Za-z0-9_-]{1,128}$/.test(reference.object_id)||!Number.isSafeInteger(reference.revision)||reference.revision<1||!open())return;
  const id=reference.object_id;selection.set(id,{object_id:id,revision:reference.revision});
  const label=document.createElement('label');label.className='context-pack-choice';label.dataset.contextMemory=id;
  const check=document.createElement('input');check.type='checkbox';check.checked=true;check.dataset.contextSelect='';
  const body=document.createElement('span');body.append(text('span',typeof event.detail.title==='string'?event.detail.title.slice(0,240):'选中的记忆'),text('small','版本 '+reference.revision+' · 仅此条记忆；预览时核对当前可用性'));
  check.addEventListener('change',()=>{invalidate();if(check.checked)selection.set(id,{object_id:id,revision:reference.revision});else selection.delete(id);$('context-pack-status').textContent='选择已改变，请重新预览。';controls();});
  label.append(check,body);$('context-pack-list').append(label);$('context-pack-more').hidden=true;
  $('context-pack-status').textContent='已选中这条记忆，正在准备预览；可刷新并重选更多记忆。';controls();prepare();
 });
 $('context-pack-close').addEventListener('click',close);dialog.addEventListener('cancel',event=>{event.preventDefault();close();});
 $('context-pack-target').addEventListener('change',()=>{invalidate();$('context-pack-status').textContent='目标已改变，请重新预览。';});
 $('context-pack-refresh').addEventListener('click',()=>{invalidate();selection.clear();cursor=null;load();});
 $('context-pack-more').addEventListener('click',()=>load(true));
 async function prepare(){
  if(busy||!validState()||!selection.size)return;invalidate();busy=true;controls();const run=epoch,task=++operation;
  try{
   const data=await api('/context-packs/preview',{target:$('context-pack-target').value,memories:[...selection.values()]});
   if(run!==epoch||!validState())return;preview=data.pack;
   renderSummary(preview);$('context-pack-text').value=preview.text;$('context-pack-preview').hidden=false;
   $('context-pack-meta').textContent=preview.payload.memories.length+' 条 · '+preview.bytes+' 字节 · 只含选中记忆及来源元数据';
   $('context-pack-status').textContent='预览已准备。复制或下载前会再次核对当前版本。';
  }catch{if(run===epoch&&validState()){invalidate();$('context-pack-status').textContent='记忆可能已更新、撤回或超出大小限制。请刷新并重新选择，不会导出部分内容。';}}
  finally{if(task===operation)busy=false;controls();}
 }
 $('context-pack-prepare').addEventListener('click',prepare);
 async function deliver(mode){
  if(busy||!preview||!validState())return;const chosen=preview,run=epoch,task=++operation;busy=true;controls();let result;
  try{
   result=await api('/context-packs/export',{selection:chosen.selection,expected_sha256:chosen.sha256,plaintext_export:true});
   if(run!==epoch||!validState())return;const fresh=result.release.pack;
   if(fresh.sha256!==chosen.sha256)throw Error('changed');
   if(mode==='copy'){
    try{await navigator.clipboard.writeText(fresh.text);if(run===epoch&&validState())$('context-pack-status').textContent='已复制这份上下文。请自行粘贴到目标 AI；Nova 尚未验证目标已收到或使用。';}
    catch{if(run===epoch&&validState()){$('context-pack-raw').open=true;$('context-pack-text').focus();$('context-pack-text').select();$('context-pack-status').textContent='浏览器未能自动复制。已选中核对后的文本，可手动复制。';}}
   }else{
    const blob=new Blob([fresh.text],{type:'text/plain;charset=utf-8'}),url=URL.createObjectURL(blob),a=document.createElement('a');
    a.href=url;a.download='nova-memory-context-'+fresh.selection.target+'.txt';a.click();setTimeout(()=>URL.revokeObjectURL(url),5000);
    $('context-pack-status').textContent='已请求下载一次性明文副本，请自行决定交给哪个 AI；它不会自动同步或撤回。';
   }
  }catch{if(run===epoch&&validState()){invalidate();$('context-pack-status').textContent='当前记忆与预览不再一致，或已不可导出。请刷新重选；没有复制或下载旧版本。';}}
  finally{result=null;if(task===operation)busy=false;controls();}
 }
 $('context-pack-copy').addEventListener('click',()=>deliver('copy'));$('context-pack-download').addEventListener('click',()=>deliver('download'));
 window.addEventListener('nova:intake-mutation',event=>{if(validState()&&!event.detail?.path?.startsWith('/context-packs/')){invalidate();$('context-pack-status').textContent='记忆库有变更，请重新预览后再使用。';}});
 new MutationObserver(()=>{if($('workspace').hidden&&dialog.open)close();}).observe($('workspace'),{attributes:true,attributeFilter:['hidden']});
})();
