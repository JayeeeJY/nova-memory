'use strict';
// Observe the running service, never treat a desired checkbox value as an active capability.
(()=>{
 const panel=$('native-settings-panel'),status=$('continuity-setting-state'),button=$('continuity-setting-refresh');let epoch=0,loading=false;
 const active=()=>!document.hidden&&!$('workspace').hidden&&!panel.hidden;
 function clear(){epoch++;status.textContent='打开设置后核对当前服务状态。';}
 async function refresh(){if(!active()||loading)return;loading=true;button.disabled=true;const e=epoch;
  try{const data=await api('/connections');if(e!==epoch||!active())return;status.textContent=data.backup_quarantined?'此副本保持隔离，任务接续不可用。':data.capabilities.continuity===true?'当前服务：任务接续已开启。':'当前服务：任务接续已关闭。';}
  catch{if(e===epoch&&active())status.textContent='暂时无法核对实际状态。请在本机 App 检查服务是否已启动。';}
  finally{loading=false;button.disabled=false;}
 }
 button.addEventListener('click',refresh);new MutationObserver(()=>{clear();if(active())refresh();}).observe(panel,{attributes:true,attributeFilter:['hidden']});
 new MutationObserver(()=>{if($('workspace').hidden)clear();else if(active())refresh();}).observe($('workspace'),{attributes:true,attributeFilter:['hidden']});
 window.addEventListener('nova:viewchange',clear);document.addEventListener('visibilitychange',()=>{clear();if(active())refresh();});window.addEventListener('pagehide',clear);
 setInterval(()=>{if(active())refresh();},5000);
})();
