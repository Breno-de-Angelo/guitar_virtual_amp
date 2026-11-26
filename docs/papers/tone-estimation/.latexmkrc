$pdf_mode = 1;
$pdflatex = 'pdflatex -interaction=nonstopmode -synctex=1 %O %S';
$bibtex_use = 2;
$out_dir = 'build';

mkdir $out_dir unless -d $out_dir;
