fn main() {
    use boa_engine::{Context, Source};
    // Extract and run exactly like __register
    let src = std::fs::read_to_string("/tmp/jett_av.js").unwrap();
    let mut src = src
        .replace(|c: char| false, ""); // noop keep
    let mut s = src.clone();
    s = s.lines().filter(|l| !l.trim_start().starts_with("import ")).collect::<Vec<_>>().join("\n");
    s = s.replace("export function", "function");
    s = s.replace("export var", "var");
    s = s.replace("export let", "let");
    s = s.replace("export const", "const");
    s = s.replace("export default ", "");
    // Provide stubs needed at compile/run
    let bootstrap = r#"
function Vec3(x,y,z){ if(!(this instanceof Vec3)) return new Vec3(x,y,z); this.x=x||0;this.y=y||0;this.z=z||0; }
function createScriptProperties(){
  var props={};
  function grab(o,d){ props[o.name]=(o.value!==undefined)?o.value:(o.options&&o.options[0]?o.options[0].value:d); }
  var b={ addSlider:function(o){grab(o,0);return b;}, addCheckbox:function(o){grab(o,false);return b;},
    addCombo:function(o){grab(o,'');return b;}, addText:function(o){grab(o,'');return b;},
    addTextInput:function(o){grab(o,'');return b;}, finish:function(){return props;} };
  return b;
}
var engine={ registerAudioBuffers:function(r){ var a=[]; for(var i=0;i<r;i++)a.push(0); return {average:a,left:a,right:a,resolution:r}; } };
var thisScene={ getLayerIndex:function(){return 0;}, createLayer:function(n){return {origin:{x:0,y:0,z:0},scale:{x:1,y:1,z:1},angles:{x:0,y:0,z:0},alpha:1,color:{x:1,y:1,z:1},parallaxDepth:0,alignment:'',perspective:false};}, sortLayer:function(){} };
var thisLayer={ scale:{x:100,y:100,z:1}, origin:{x:0,y:0,z:0}, angles:{x:0,y:0,z:0}, alpha:1, color:{x:1,y:1,z:1}, parallaxDepth:0, visible:true };
"#;
    let mut ctx = Context::default();
    ctx.eval(Source::from_bytes(bootstrap.as_bytes())).unwrap();
    let body = format!(
        "{s}\n;return {{update: (typeof update !== 'undefined') ? update : null, init: (typeof init !== 'undefined') ? init : null}};"
    );
    let code = format!("var h = (new Function({}))(); 'registered'", serde_json::to_string(&body).unwrap());
    match ctx.eval(Source::from_bytes(code.as_bytes())) {
        Ok(v) => println!("register: {}", v.display()),
        Err(e) => { println!("register err: {e}"); return; }
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ctx.eval(Source::from_bytes(b"h.init(); 'inited'"))
    })) {
        Ok(Ok(v)) => println!("init: {}", v.display()),
        Ok(Err(e)) => println!("init err: {e}"),
        Err(_) => println!("init PANICKED"),
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ctx.eval(Source::from_bytes(b"h.update(true); 'updated'"))
    })) {
        Ok(Ok(v)) => println!("update: {}", v.display()),
        Ok(Err(e)) => println!("update err: {e}"),
        Err(_) => println!("update PANICKED"),
    }
}
