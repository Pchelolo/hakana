use namespace Facebook\XHP\{Core as x, HTML};
use type Facebook\XHP\HTML\{doctype, html};
use namespace HH\Lib\Str;

final xhp class MyElement extends x\element {
	attribute
		string a = '',
		string b = '';
	
	public function foo() {
		echo $this->:b;
	}
}

<<__EntryPoint>>
function bar(): void {
	$a = <MyElement />;
	$a->foo();
}