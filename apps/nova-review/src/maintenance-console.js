'use strict';
// Bounded Owner actions; no automatic scheduling, external model or browser persistence.
(()=>{
 const panel=$('maintenance-panel'),status=$('maintenance-status'),list=$('maintenance-jobs');
 let epoch=0,busy=false,requestId=crypto.randomUUID(),previewed=false;
 const names={pending:'等待执行',cancelled:'已取消',stale:'材料版本已变化或预览到期，请重新检查',completed:'本批维护已完成'};
 const reasons={compiler_busy:'整理任务正在运行，本批维护已延后。',idle_unknown_scan_limit:'当前任务量超出本轮空闲检查范围，维护暂缓。'};
 function summary(p){return '更新 '+p.rebuild.length+' 个主题视图，清理 '+p.evict.length+' 个过期主题缓存。原始材料、记忆版本和用户修正保留。';}
 function clear(){epoch++;previewed=false;requestId=crypto.randomUUID();status.textContent='';list.replaceChildren();$('maintenance-schedule').hidden=true;}
 async function refresh(){
  if($('workspace').hidden||panel.closest('.view').hidden)return;
  const e=epoch,data=await api('/maintenance/jobs');if(e!==epoch||$('workspace').hidden)return;list.replaceChildren();
  for(const job of data.items.slice(0,10)){
   const row=document.createElement('article');row.className='record';row.append(text('h3',names[job.state]||'维护状态待检查'),text('p',job.state==='completed'?'已更新 '+job.pages_rebuilt+' 个主题，清理 '+job.pages_pruned+' 个缓存；超限暂停 '+job.pages_blocked+' 个。':summary(job.preview)),text('small',new Date(job.updated_at).toLocaleString('zh-CN')));
   if(reasons[job.reason])row.append(text('p',reasons[job.reason]));
   if(job.state==='pending')for(const [action,label] of [['run','空闲时执行本批'],['cancel','取消这批维护']]){
    const button=text('button',label);button.className='secondary';button.type='button';button.addEventListener('click',()=>perform(async()=>{const updated=await api('/maintenance/jobs/'+encodeURIComponent(job.id)+'/'+action,{});status.textContent=reasons[updated.reason]||names[updated.state];await refresh();if(typeof loadNative==='function')await loadNative(true);}));row.append(button);
   }
   list.append(row);
  }
  if(!data.items.length)list.append(text('p','尚无维护记录。'));
 }
 async function perform(action){if(busy)return;busy=true;for(const b of panel.querySelectorAll('button'))b.disabled=true;try{await action();}catch{status.textContent='本次操作尚未确认，请刷新记录后再试。'}finally{busy=false;for(const b of panel.querySelectorAll('button'))b.disabled=false;}}
 $('maintenance-preview').addEventListener('click',()=>perform(async()=>{const e=epoch,p=await api('/maintenance/preview',{});if(e!==epoch||$('workspace').hidden)return;previewed=true;status.textContent=summary(p)+' 本轮不调用模型。';$('maintenance-schedule').hidden=false;await refresh();}));
 $('maintenance-schedule').addEventListener('click',()=>perform(async()=>{if(!previewed)return;const e=epoch,job=await api('/maintenance/jobs',{request_id:requestId});if(e!==epoch||$('workspace').hidden)return;requestId=crypto.randomUUID();previewed=false;$('maintenance-schedule').hidden=true;status.textContent='本批计划已固定。'+summary(job.preview)+' 可以执行或取消。';await refresh();}));
 $('maintenance-restore').addEventListener('click',()=>perform(async()=>{const e=epoch,job=await api('/maintenance/jobs',{request_id:crypto.randomUUID(),policy:{max_rebuild_pages:4,max_prune_pages:0,derived_ttl_seconds:604800,prune_before:null,include_cold:true}});if(e!==epoch||$('workspace').hidden)return;status.textContent='恢复计划已准备。'+summary(job.preview)+' 点击执行后重新生成展示缓存。';await refresh();}));
 $('maintenance-refresh').addEventListener('click',()=>perform(refresh));
 window.addEventListener('nova:viewchange',()=>refresh().catch(()=>{}));
 new MutationObserver(()=>{if($('workspace').hidden)clear();else refresh().catch(()=>{});}).observe($('workspace'),{attributes:true,attributeFilter:['hidden']});
 window.addEventListener('pagehide',clear);
})();
