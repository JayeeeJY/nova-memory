'use strict';
const $=id=>document.getElementById(id);
let token='',config=null,requestId=crypto.randomUUID(),saving=false,nextBefore=null,compilerAvailable=false,backupQuarantined=false,unitsBefore=null,unitSignature='',jobSignature='';
let workspaceAccess=null;
const text=(tag,value)=>{const el=document.createElement(tag);el.textContent=value;return el;};
const messages={NOVA_IDEMPOTENCY_CONFLICT:'同一次接收的内容发生变化，请重新编辑后保存。',NOVA_REVIEW_BOUNDARY_DENIED:'连接已锁定，请重新解锁。'};
const jobFailureMessages={
 model_unavailable:'暂时无法使用整理模型。确认模型服务正在运行、地址和模型名称正确后，再重新整理。',
 invalid_output:'模型返回的结果未通过校验。你可以重新整理，或换一个可用的整理模型。',
 worker_interrupted:'整理被中断。服务恢复后，可以重新整理。',
 configuration_changed:'整理期间模型设置发生变化。可以使用当前设置重新整理。',
 budget_exceeded:'本库已达到整理调用上限。请检查运行设置中的模型调用额度。'
};
async function api(path,body,download=false){
 const headers={'X-Nova-Review':'owner-review'};
 if(token)headers.Authorization='Bearer '+token;
 const options={headers,cache:'no-store',credentials:'same-origin'};
 if(body!==undefined){options.method='POST';headers['Content-Type']='application/json';options.body=JSON.stringify(body);}
 const response=await fetch('/console/api/owner/intake'+path,options);
 if(!response.ok){let data;try{data=await response.json();}catch{}if(data?.error?.code==='NOVA_REVIEW_BOUNDARY_DENIED'&&$('workspace-access-form')){token='';$('workspace').hidden=true;$('native-settings-panel').hidden=true;$('unlock').dataset.credentialUnlock='required';$('unlock').hidden=false;for(const dialog of document.querySelectorAll('dialog[open]'))dialog.close();}const error=new Error(messages[data?.error?.code]||data?.error?.message||'连接未完成，请保留输入并重试。');error.code=data?.error?.code;throw error;}
 const result=await(download?response.blob():response.json());
 if(body!==undefined)window.dispatchEvent(new CustomEvent('nova:intake-mutation',{detail:{path}}));
 return result;
}
function selectionFields(){const location=$('location').value;$('model-fields').hidden=location==='unconfigured';$('remote-consent').hidden=location!=='remote';$('model-id').required=location!=='unconfigured';$('endpoint').required=location!=='unconfigured';$('allow-source').required=location==='remote';$('location-hint').textContent=location==='remote'?'仅限每次选定的材料。凭据在模型运行环境中配置，不在这里填写。':'连接本机地址；实际处理位置以该模型服务配置为准。';}
function modelNotice(){const fixture=/^(synthetic|mock|fixture)/i.test(config?.selection?.model_id||'');$('fixture-notice').hidden=!fixture;$('fixture-notice').textContent=fixture?'当前为合成流程演练：固定响应模拟器，不是语言模型。请使用测试材料。':'';}
function showConfig(data){config=data.configuration;compilerAvailable=data.compiler_available;backupQuarantined=data.backup_quarantined===true;modelNotice();$('location').value=config.selection.location;$('model-id').value=config.selection.model_id||'';$('endpoint').value=config.selection.endpoint||'';$('allow-source').checked=config.selection.allow_selected_source===true;selectionFields();$('processing-status').textContent=backupQuarantined?'此备份副本保持隔离；后续删除与撤权尚未核对，不执行模型整理或向 AI 提供记忆。':compilerAvailable?'自动整理已启用。每次只处理所选材料；纠正时附带所选记忆。每次整理有等待上限，本库最多 '+data.maximum_calls+' 次模型调用。':'整理服务尚未启用。当前先加密保留原文，设置不会触发发送。';if(data.temporary_demo)$('storage-label').textContent='临时合成演示 · 关闭服务后销毁';else if(data.demo)$('storage-label').textContent='独立合成验收库 · 重启仍保留';}
async function loadJobs(append=false){
 await loadWithdrawals();
 const data=await api('/jobs'+(append&&nextBefore?'?before='+encodeURIComponent(nextBefore):''));
 if(!append&&jobSignature===JSON.stringify(data))return;if(!append){jobSignature=JSON.stringify(data);$('jobs').replaceChildren();}
 $('queue-hint').textContent=data.processing_blocker==='backup_quarantined'?'隔离的备份副本：材料可供本人查看，整理与 AI 连接保持关闭。':data.processing_blocker==='model_unconfigured'?'材料已保存，等待配置模型后整理。':data.processing_blocker?'材料已保存，等待整理服务就绪。':'自动整理进行中；失败时原文会保留，可重新整理。';
 for(const record of data.items){
  const article=document.createElement('article');article.className='record';article.dataset.jobId=record.job.job_id;
  const head=document.createElement('div');head.className='record-head';
  const labels={received:'已保存 · 等待整理',processing:'正在整理',failed:'原文已保留 · 整理失败',ready:'整理完成'};
  const label=text('span',record.retry_requested&&record.job.state==='failed'?'原文已保留 · 等待重试':record.job.state==='ready'&&record.compilation_summary?.no_op?'整理完成 · 未提取长期记忆':labels[record.job.state]);label.className='badge';
  const time=text('time',new Date(record.receipt.saved_at).toLocaleString('zh-CN',{hour12:false}));time.dateTime=record.receipt.saved_at;head.append(label,time);article.append(head);
  if(record.job.state==='failed'&&!record.retry_requested){
   const reason=Object.hasOwn(jobFailureMessages,record.job.error_code)?jobFailureMessages[record.job.error_code]:'整理未完成。你可以查看原文，确认整理服务就绪后再试。';
   article.append(text('p',reason));
  }
  const details=document.createElement('details'),summary=text('summary','查看原文与接收记录');details.append(summary);
  const controls=document.createElement('div');controls.className='source-controls';
  for(const [action,label] of [['withdraw','撤回此材料'],['delete','删除此材料']]){const button=text('button',label);button.type='button';button.className='secondary';button.addEventListener('click',async()=>{
   const warning=action==='delete'?'删除此材料及包含它的本库派生记录？已有备份和其他设备副本需要另外删除。':'撤回后，相关记忆版本与 Wiki 将停止使用这份材料；原文仍加密保留。继续？';
   if(!window.confirm(warning))return;button.disabled=true;try{await api('/sources/'+encodeURIComponent(record.job.source.object_id)+'/'+action,{});jobSignature='';unitSignature='';wikiSignature='';await loadJobs();await loadUnits();await loadWiki();$('notice').textContent=action==='delete'?'已从当前摄入库删除。已有备份与设备副本需另行处理。':'材料已撤回，相关理解正在重新整理。';}catch(e){$('notice').textContent=e.message;button.disabled=false;}
  });controls.append(button);}details.append(controls);
  let loaded=false;details.addEventListener('toggle',async()=>{if(!details.open||loaded)return;try{const source=await api('/sources/'+encodeURIComponent(record.job.source.object_id));details.append(text('pre',source.content),text('small','接收记录 '+record.job.job_id),text('small','原文版本 '+source.source.revision+' · 处理尝试 '+record.job.attempt+' 次'));loaded=true;}catch(e){$('notice').textContent=e.message;}});article.append(details);
  if(record.job.state==='failed'&&!record.retry_requested&&record.job.attempt<5){const retry=text('button','重新整理');retry.type='button';retry.className='secondary';retry.addEventListener('click',async()=>{retry.disabled=true;try{await api('/jobs/'+encodeURIComponent(record.job.job_id)+'/retry',{});await loadJobs();await loadUnits();await loadWiki();}catch(e){$('notice').textContent=e.message;retry.disabled=false;}});article.append(retry);}
  $('jobs').append(article);
 }
 if(!append&&!data.items.length)$('jobs').append(text('p','还没有材料。写下第一条值得保留的内容吧。'));
 nextBefore=data.next_before;$('more').hidden=!nextBefore;
}
async function enter(){const data=await api('/configuration');showConfig(data);$('capture-form').hidden=backupQuarantined;if($('global-capture'))$('global-capture').hidden=backupQuarantined;if(backupQuarantined)$('storage-label').textContent='加密备份检查';$('unlock').hidden=true;$('workspace').hidden=false;$('lock').hidden=!token;$('owner-token').value='';if($('workspace-access-form'))await loadWorkspaceAccess();await loadJobs();await loadUnits();await loadWiki();}
$('unlock-form').addEventListener('submit',async e=>{e.preventDefault();token=$('owner-token').value.trim();try{await enter();$('unlock-status').textContent='';}catch{$('unlock-status').textContent='无法解锁，请核对 Owner 凭据与摄入库是否已启用。';token='';}});
$('lock').addEventListener('click',()=>{token='';location.reload();});
$('content').addEventListener('input',()=>{requestId=crypto.randomUUID();$('save-status').textContent='';});
$('capture-form').addEventListener('submit',async e=>{
 e.preventDefault();if(backupQuarantined||saving||!$('content').value.trim())return;
 if(new TextEncoder().encode($('content').value).length>65536){$('save-status').textContent='材料超过 64 KiB，请分成几段保存。';return;}
 const payload={schema_version:'nova.intake.v0.1',request_id:requestId,source_platform:'web-console',capture_fidelity:'f2_visible_transcript',content:$('content').value,occurred_at:null};
 if(new TextEncoder().encode(JSON.stringify(payload)).length>96*1024){$('save-status').textContent='材料经编码后超过单次接收限制，请分段保存。';return;}
 saving=true;$('save').disabled=true;$('content').disabled=true;$('notice').textContent='';$('save-status').textContent='正在加密保存…';
 try{await api('',payload);$('content').value='';requestId=crypto.randomUUID();$('save-status').textContent='已保存，等待整理。原文已经留在你的记忆空间。';try{await loadJobs();}catch{$('notice').textContent='保存已确认，列表暂未刷新，可稍后刷新查看。';}}
 catch(e){$('save-status').textContent=e.message+' 输入已保留，可直接重试。';}
 finally{saving=false;$('save').disabled=false;$('content').disabled=false;}
});
$('location').addEventListener('change',selectionFields);
$('model-form').addEventListener('submit',async e=>{e.preventDefault();if(!config)return;const selection={location:$('location').value};if(selection.location!=='unconfigured'){selection.model_id=$('model-id').value.trim();selection.endpoint=$('endpoint').value.trim();if(selection.location==='remote')selection.allow_selected_source=$('allow-source').checked;}
 const button=e.target.querySelector('button');button.disabled=true;try{config=await api('/configuration',{expected_revision:config.revision,selection});modelNotice();$('model-status').textContent=compilerAvailable?'设置已保存，队列会自动使用这个模型整理材料。':'设置已保存。原文保持加密保存，等待整理服务。';await loadJobs();}catch(e){$('model-status').textContent=e.message;try{showConfig(await api('/configuration'));}catch{}}finally{button.disabled=false;}});
$('refresh').addEventListener('click',()=>loadJobs().then(()=>loadUnits()).then(()=>loadWiki()).catch(e=>{$('notice').textContent=e.message;}));
$('more').addEventListener('click',()=>loadJobs(true).catch(e=>{$('notice').textContent=e.message;}));
$('backup').addEventListener('click',async()=>{$('backup').disabled=true;try{const blob=await api('/backup',{},true),url=URL.createObjectURL(blob),a=document.createElement('a');a.href=url;a.download='nova-intake-encrypted.sqlite';a.click();setTimeout(()=>URL.revokeObjectURL(url),10000);$('backup-status').textContent='加密备份已生成，请确认另存并单独保管原 Vault 密钥。恢复副本会先保持隔离。';}catch(e){$('backup-status').textContent=e.message;}finally{$('backup').disabled=false;}});
window.addEventListener('beforeunload',e=>{if($('content').value||saving||[...document.querySelectorAll('#units textarea')].some(n=>n.value)){e.preventDefault();e.returnValue='';}});
async function openLocalSession(){const response=await fetch('/console/local-session',{method:'POST',headers:{'X-Nova-Review':'owner-review','Content-Type':'application/json'},credentials:'same-origin',cache:'no-store',body:'{}'});if(!response.ok)throw Error('local_entry_failed');}
async function loadWorkspaceAccess(){
 workspaceAccess=(await api('/workspace-access')).settings;
 $('credential-unlock-enabled').checked=workspaceAccess.credential_unlock_enabled;
 $('credential-unlock-enabled').disabled=false;$('workspace-access-save').disabled=false;accessFields();
}
function accessFields(){if(!$('workspace-access-form'))return;const required=$('credential-unlock-enabled').checked&&!workspaceAccess?.credential_unlock_enabled&&!token;$('workspace-access-credential-field').hidden=!required;$('workspace-access-credential').required=required;if(!required)$('workspace-access-credential').value='';}
if($('workspace-access-form')){
 $('credential-unlock-enabled').addEventListener('change',accessFields);
 $('workspace-access-form').addEventListener('submit',async event=>{
  event.preventDefault();if(!workspaceAccess)return;const button=$('workspace-access-save');button.disabled=true;
  const body={expected_revision:workspaceAccess.revision,credential_unlock_enabled:$('credential-unlock-enabled').checked};
  if(!$('workspace-access-credential-field').hidden)body.owner_credential=$('workspace-access-credential').value.trim();
  $('workspace-access-credential').value='';
  try{await api('/workspace-access',body);token='';location.reload();}
  catch{$('workspace-access-status').textContent='设置未保存。开启解锁时请核对独立凭据，再重试。';await loadWorkspaceAccess().catch(()=>{});}
  finally{body.owner_credential='';button.disabled=false;}
 });
}
(async()=>{try{const session=await(await fetch('/console/session',{cache:'no-store'})).json();if(!session.capabilities.automatic_intake){$('notice').textContent='当前工作台尚未启用一键摄入库。请使用已初始化的新摄入库启动服务。';return;}
 if(session.capabilities.native_workspace&&session.credential_unlock_enabled===false){await openLocalSession();await enter();}
 else if(session.requires_owner_token){if(session.capabilities.native_workspace)$('unlock').dataset.credentialUnlock='required';$('unlock').hidden=false;}
 else await enter();
 }catch{$('notice').textContent='本机服务暂不可用，请检查启动状态。';}})();

let unitsLoading=false;
let wikiBefore=null,wikiSignature="",withdrawalBefore=null,withdrawalSignature="";
function valueText(value){return typeof value==='string'?value:JSON.stringify(value);}
async function loadUnits(append=false){
 if(unitsLoading)return;unitsLoading=true;
 try{
  const parameters=new URLSearchParams();if(append&&unitsBefore)parameters.set('before',unitsBefore);const search=document.getElementById('library-search')?.value.trim();if(search)parameters.set('q',search);
  const data=await api('/units'+(parameters.size?'?'+parameters:''));
  if(!append&&unitSignature===JSON.stringify(data))return;
  if(!append&&[...document.querySelectorAll('#units textarea')].some(n=>n.value))return;
  if(!append){unitSignature=JSON.stringify(data);$('units').replaceChildren();}
  for(const item of data.items){
   const memory=item.memory,u=memory.unit,article=document.createElement('article');article.className='record';article.dataset.unitId=u.header.object.object_id;article.dataset.revision=u.header.object.revision;
   article.append(text('p',u.content.subject+' · '+u.content.predicate.split(':').pop().replaceAll('_',' ')+'：'+valueText(u.content.value)));
   article.append(text('small',(memory.protected?'按你的纠正形成的版本 · ':'')+(u.origin==='inferred'?'AI 推断':'AI 提取')+' · 语义支持尚未独立验证 · 版本 '+u.header.object.revision));
   if(u.content.polarity==='negative')article.append(text('small','否定陈述'));
   if(u.content.modality!=='asserted')article.append(text('small',({planned:'计划',hypothetical:'假设',unknown:'尚不确定'})[u.content.modality]));
   if(Object.keys(u.content.conditions).length)article.append(text('p','适用条件：'+Object.entries(u.content.conditions).map(([k,v])=>k+'：'+valueText(v)).join('；')));
   if(document.getElementById('context-pack-dialog')&&!backupQuarantined&&u.header.state==='active'&&['private','shareable'].includes(u.header.privacy_level)){
    const use=text('button','在其他 AI 中使用这条');use.type='button';use.className='secondary';use.dataset.unitContext='';
    use.addEventListener('click',()=>window.dispatchEvent(new CustomEvent('nova:context-pack-open',{detail:{reference:{...u.header.object},title:(({user:'关于我',self:'关于我'})[u.content.subject]||u.content.subject)+' · '+u.content.predicate.split(':').pop().replaceAll('_',' ')+'：'+valueText(u.content.value)}})));
    article.append(use);
   }
   if(data.continuity_available===true&&!backupQuarantined&&u.header.state==='active'&&['private','shareable'].includes(u.header.privacy_level)){
    const task=text('button','从这条记忆开始任务');task.type='button';task.className='secondary';task.dataset.unitTask='';task.addEventListener('click',()=>window.dispatchEvent(new CustomEvent('nova:task-open',{detail:{memory:{...u.header.object},title:u.content.subject+' · '+valueText(u.content.value)}})));article.append(task);
   }
   const details=document.createElement('details');details.append(text('summary','查看依据与版本'));for(const ev of u.evidence)details.append(text('blockquote',ev.quote));
   details.append(text('small',u.valid_time.kind==='unknown'?'适用时间未说明':u.valid_time.start+' 至 '+(u.valid_time.end||'未说明结束')));
   const versionButton=text('button','查看历史版本');versionButton.className='secondary';versionButton.type='button';versionButton.addEventListener('click',async()=>{versionButton.disabled=true;try{const history=await api('/units/'+encodeURIComponent(u.header.object.object_id)+'/history');for(const old of history.versions)details.append(text('p','版本 '+old.header.object.revision+'：'+valueText(old.content.value)));}catch(e){$('notice').textContent=e.message;versionButton.disabled=false;}});details.append(versionButton);article.append(details);
   const correction=document.createElement('details');correction.append(text('summary','告诉 Nova 如何纠正'));const form=document.createElement('form'),label=text('label','哪里不准确，或只适用于什么情况？'),input=document.createElement('textarea');input.rows=2;input.maxLength=4000;input.required=true;input.id='correct-'+u.header.object.object_id;label.htmlFor=input.id;
   const submit=text('button','交给 Nova 纠正');submit.type='submit';submit.disabled=!compilerAvailable;const status=text('p','');status.setAttribute('role','status');let correctionId=crypto.randomUUID();input.addEventListener('input',()=>{correctionId=crypto.randomUUID();});
   form.append(label,input,submit,status);form.addEventListener('submit',async e=>{e.preventDefault();submit.disabled=true;input.disabled=true;try{await api('/units/'+encodeURIComponent(u.header.object.object_id)+'/correct',{request_id:correctionId,expected_revision:u.header.object.revision,instruction:input.value});input.value='';correctionId=crypto.randomUUID();status.textContent='纠正说明已保存，Nova 正在整理新版本。';await loadJobs();}catch(e){status.textContent=e.message;if(e.code==='NOVA_INVALID_STATE'){const draft=input.value;input.value='';unitSignature='';await loadUnits();await loadWiki();const replacement=document.getElementById('correct-'+u.header.object.object_id);if(replacement){replacement.value=draft;replacement.closest('details').open=true;const message=replacement.closest('form').querySelector('[role=status]');message.textContent='记忆已有新版本。已保留你的说明，请查看更新后的内容再提交。';}}}finally{submit.disabled=!compilerAvailable;input.disabled=false;}});correction.append(form);article.append(correction);
   if(item.can_undo){const undo=text('button','撤销最近纠正');undo.type='button';undo.className='secondary';undo.addEventListener('click',async()=>{undo.disabled=true;try{await api('/units/'+encodeURIComponent(u.header.object.object_id)+'/undo',{expected_revision:u.header.object.revision});unitSignature='';await loadUnits();await loadWiki();}catch(e){$('notice').textContent=e.message;undo.disabled=false;}});article.append(undo);}
   $('units').append(article);
  }
  if(!append&&!data.items.length)$('units').append(text('p',search?'没有找到匹配的整理内容。':'整理完成后，带有原文依据的记忆会显示在这里。'));
  unitsBefore=data.next_before;$('units-more').hidden=!unitsBefore;
 }finally{unitsLoading=false;}
}
$('units-more').addEventListener('click',()=>loadUnits(true).catch(e=>{$('notice').textContent=e.message;}));
setInterval(()=>{if(!$('workspace').hidden&&!document.hidden){loadJobs().then(()=>loadUnits()).then(()=>loadWiki()).catch(e=>{$('notice').textContent=e.message;});}},2000);

async function loadWiki(append=false){
 const data=await api('/wiki'+(append&&wikiBefore?'?before='+wikiBefore:''));
 if(!append&&wikiSignature===JSON.stringify(data))return;
 if(!append){wikiSignature=JSON.stringify(data);$('wiki').replaceChildren();}
 $('wiki-status').textContent=data.queue.blocked?'有 '+data.queue.blocked+' 个主题超出当前整理范围，尚未完成；原文与逐条记忆已保留。':data.queue.pending?'正在更新 '+data.queue.pending+' 个主题…':data.queue.cold?'有 '+data.queue.cold+' 个主题缓存已清理，逐条记忆仍可读取。可在 AI 整理的本地维护中重新生成。':'Wiki 已按当前材料版本更新。关联来自结构规则，语义仍待核对。';
 for(const page of data.items){
  const article=document.createElement('article');article.className='record wiki-page';article.dataset.wikiId=page.id;
  const subject=({user:'关于我',self:'关于我'})[page.subject]||page.subject;
  article.append(text('h3',subject),text('small',page.memories.length+' 条记忆 · 视图更新 '+page.generation));
  for(const memory of page.memories){const card=document.createElement('section');card.className='wiki-memory';card.dataset.memoryId=memory.id;
   card.append(text('p',valueText(memory.content.value)));
   const sources=new Set(memory.evidence.map(e=>e.source.object_id));
   card.append(text('small','来源角色：'+memory.source_roles.map(r=>({user:'用户',assistant:'AI 对话',external:'外部材料',agent:'Agent',mixed:'混合来源'})[r]||r).join('、')));
   card.append(text('small',sources.size+' 份来源 · '+memory.members.length+' 条提取记录'+(memory.protected_members.length?' · 保留你的纠正':'')+' · AI 陈述尚未独立验证'));
   if(memory.content.polarity==='negative')card.append(text('small','否定陈述'));
   if(memory.content.modality!=='asserted')card.append(text('small','陈述性质：'+memory.content.modality));
   if(Object.keys(memory.content.conditions).length)card.append(text('small','适用条件：'+Object.entries(memory.content.conditions).map(([k,v])=>(({'scope:context':'场景'})[k]||k.split(':').pop().replaceAll('_',' '))+'：'+valueText(v)).join('；')));
   card.append(text('small',memory.valid_time.kind==='unknown'?'适用时间未说明':memory.valid_time.start+' 至 '+(memory.valid_time.end||'未说明结束')));
   const evidence=document.createElement('details');evidence.append(text('summary','查看 '+sources.size+' 份来源依据'));
   for(const ev of memory.evidence){evidence.append(text('blockquote',ev.quote),text('small',ev.source.object_id+' · 原文版本 '+ev.source.revision));}
   card.append(evidence);article.append(card);
  }
  const kinds={parallel_evidence:'同一陈述，保留不同来源、隐私或时间的记录',conditional_variant:'条件不同，分别保留',temporal_transition:'适用时间先后变化',potential_conflict:'差异待核对，尚不确定是否冲突'};
  for(const kind of new Set(page.associations.map(r=>r.kind)))article.append(text('p',kinds[kind]));
  const lineage=document.createElement('details');lineage.append(text('summary','视图依据与重建'));lineage.append(text('small','原始主题标识 '+page.predicate));lineage.append(text('small','规则 '+page.rules_version+' · '+page.inputs.length+' 个输入版本 · '+new Date(page.updated_at).toLocaleString('zh-CN')));
  const rebuild=text('button','重新整理这个主题');rebuild.type='button';rebuild.className='secondary';rebuild.addEventListener('click',async()=>{rebuild.disabled=true;try{await api('/wiki/'+encodeURIComponent(page.id)+'/rebuild',{});wikiSignature='';await loadWiki();}catch(e){$('notice').textContent=e.message;rebuild.disabled=false;}});lineage.append(rebuild);article.append(lineage);$('wiki').append(article);
 }
 if(!append&&!data.items.length)$('wiki').append(text('p',data.queue.pending?'新材料已进入整理队列，更新后会显示在这里。':data.queue.cold?'主题缓存已清理；逐条记忆和原件仍然保留。':'材料整理后，项目与主题的理解会在这里逐渐形成。'));
 wikiBefore=data.next_before;$('wiki-more').hidden=!wikiBefore;
}
$('wiki-more').addEventListener('click',()=>loadWiki(true).catch(e=>{$('notice').textContent=e.message;}));

async function loadWithdrawals(append=false){
 const data=await api('/withdrawals'+(append&&withdrawalBefore?'?before='+withdrawalBefore:''));if(!append&&withdrawalSignature===JSON.stringify(data))return;if(!append){withdrawalSignature=JSON.stringify(data);$('withdrawals').replaceChildren();}
 for(const item of data.items){const record=document.createElement('article');record.className='record';record.append(text('small',item.source_id+' · '+new Date(item.withdrawn_at).toLocaleString('zh-CN')));
 const button=text('button','删除已撤回的材料');button.type='button';button.className='secondary';button.addEventListener('click',async()=>{if(!window.confirm('删除本库中的材料与相关派生记录？已有备份与其他设备副本需另行删除。'))return;button.disabled=true;try{await api('/sources/'+encodeURIComponent(item.source_id)+'/delete',{});await loadWithdrawals();$('notice').textContent='已从当前摄入库删除。已有备份与设备副本需另行处理。';}catch(e){$('notice').textContent=e.message;button.disabled=false;}});record.append(button);$('withdrawals').append(record);}
 if(!append&&!data.items.length)$('withdrawals').append(text('p','没有仍待保管或删除的已撤回材料。'));withdrawalBefore=data.next_before;$('withdrawals-more').hidden=!withdrawalBefore;
}
$('withdrawals-more').addEventListener('click',()=>loadWithdrawals(true).catch(e=>{$('notice').textContent=e.message;}));
